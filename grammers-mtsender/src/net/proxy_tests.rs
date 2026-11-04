// Copyright 2026 - developers of the `grammers` project.
// Licensed under the Apache License, Version 2.0 or the MIT license, at your option.

use std::io::ErrorKind;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

use super::{NetStream, ServerAddr};

const DEADLINE: Duration = Duration::from_secs(5);

async fn read_request(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        assert!(bytes.len() < 4096);
        bytes.push(stream.read_u8().await.unwrap());
    }
    String::from_utf8(bytes).unwrap()
}

fn proxied(address: SocketAddr, proxy: SocketAddr, userinfo: &str) -> ServerAddr {
    ServerAddr::Proxied {
        address,
        proxy: format!("http://{userinfo}{proxy}"),
    }
}

#[tokio::test]
async fn http_tunnel_preserves_coalesced_bytes_and_bidirectional_io() {
    timeout(DEADLINE, async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = "192.0.2.1:443".parse().unwrap();
        let addr = proxied(target, listener.local_addr().unwrap(), "");
        let server = async {
            let (mut socket, _) = listener.accept().await.unwrap();
            assert_eq!(
                read_request(&mut socket).await,
                "CONNECT 192.0.2.1:443 HTTP/1.1\r\nHost: 192.0.2.1:443\r\n\r\n"
            );
            // Any 2xx establishes the tunnel; framing headers have no meaning.
            socket
                .write_all(b"HTTP/1.1 204 Tunnel\r\nContent-Length: 999\r\n\r\n\0\xffhello")
                .await
                .unwrap();
            let mut reply = [0; 4];
            socket.read_exact(&mut reply).await.unwrap();
            assert_eq!(&reply, b"ping");
        };
        let client = async {
            let mut stream = NetStream::connect(&addr).await.unwrap();
            let NetStream::Tcp(ref socket) = stream else {
                panic!("HTTP should use an ordinary TCP stream")
            };
            assert!(socket.nodelay().unwrap());
            let (mut reader, mut writer) = stream.split();
            let mut greeting = [0; 7];
            reader.read_exact(&mut greeting).await.unwrap();
            assert_eq!(&greeting, b"\0\xffhello");
            writer.write_all(b"ping").await.unwrap();
            stream.disconnect().await.unwrap();
        };
        tokio::join!(client, server);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn http_basic_auth_decodes_url_credentials_and_formats_ipv6_target() {
    timeout(DEADLINE, async {
        for (userinfo, expected) in [
            ("user:pass@", "dXNlcjpwYXNz"),
            ("us%40er:p%3Aa%25ss@", "dXNAZXI6cDphJXNz"),
            ("user@", "dXNlcjo="),
            (":pass@", "OnBhc3M="),
            ("u%2Bser:p%20ass@", "dStzZXI6cCBhc3M="),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = proxied("[2001:db8::1]:443".parse().unwrap(), listener.local_addr().unwrap(), userinfo);
            let server = async {
                let (mut socket, _) = listener.accept().await.unwrap();
                assert_eq!(read_request(&mut socket).await, format!(
                    "CONNECT [2001:db8::1]:443 HTTP/1.1\r\nHost: [2001:db8::1]:443\r\nProxy-Authorization: Basic {expected}\r\n\r\n"));
                socket.write_all(b"HTTP/1.0 200 Connection established\r\n\r\n").await.unwrap();
            };
            let client = async { NetStream::connect(&addr).await.unwrap(); };
            tokio::join!(client, server);
        }
    }).await.unwrap();
}

#[tokio::test]
async fn http_response_handles_every_split_and_informational_headers() {
    timeout(DEADLINE, async {
        let response = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nX-Test: value\r\n\r\n";
        for split in 1..response.len() {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = proxied(
                "127.0.0.1:9".parse().unwrap(),
                listener.local_addr().unwrap(),
                "",
            );
            let server = async {
                let (mut socket, _) = listener.accept().await.unwrap();
                read_request(&mut socket).await;
                socket.write_all(&response[..split]).await.unwrap();
                tokio::time::sleep(Duration::from_millis(1)).await;
                socket.write_all(&response[split..]).await.unwrap();
                socket.write_all(b"tail").await.unwrap();
            };
            let client = async {
                let mut stream = NetStream::connect(&addr).await.unwrap();
                let (mut reader, _) = stream.split();
                let mut bytes = [0; 4];
                reader.read_exact(&mut bytes).await.unwrap();
                assert_eq!(&bytes, b"tail");
            };
            tokio::join!(client, server);
        }
    })
    .await
    .unwrap();
}

async fn rejected_response(response: &[u8], kind: ErrorKind) -> String {
    timeout(DEADLINE, async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        // A listening target detects accidental direct fallback after rejection.
        let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = proxied(
            target.local_addr().unwrap(),
            listener.local_addr().unwrap(),
            "synthetic:secret@",
        );
        let server = async {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request(&mut socket).await;
            // An early rejection may close the client before the whole response.
            let _ = socket.write_all(response).await;
        };
        let client = async {
            let Err(error) = NetStream::connect(&addr).await else {
                panic!("proxy unexpectedly accepted response")
            };
            assert_eq!(error.kind(), kind);
            let message = format!("{error:?}");
            for secret in ["synthetic", "secret", "c3ludGhldGljOnNlY3JldA=="] {
                assert!(!message.contains(secret), "error leaked proxy credentials");
            }
            message
        };
        let (error, ()) = tokio::join!(client, server);
        assert!(
            timeout(Duration::from_millis(10), target.accept())
                .await
                .is_err()
        );
        error
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn http_rejects_errors_eof_and_malformed_responses_without_leaking_credentials() {
    assert!(
        rejected_response(
            b"HTTP/1.1 407 synthetic secret\r\nProxy-Authenticate: Basic realm=secret\r\n\r\n",
            ErrorKind::PermissionDenied
        )
        .await
        .contains("407")
    );
    for status in [301, 403, 502] {
        assert!(
            rejected_response(
                format!("HTTP/1.1 {status} secret\r\n\r\n").as_bytes(),
                ErrorKind::ConnectionRefused
            )
            .await
            .contains(&status.to_string())
        );
    }
    for response in [
        b"".as_slice(),
        b"HTTP/1.1 200",
        b"HTTP/1.1 200 OK\r\nX: test\r\n",
    ] {
        rejected_response(response, ErrorKind::UnexpectedEof).await;
    }
    for response in [
        b"not HTTP\r\n\r\n".as_slice(),
        b"HTTP/1.1 abc Invalid\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nInvalid header\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nX: a\0b\r\n\r\n",
    ] {
        rejected_response(response, ErrorKind::InvalidData).await;
    }
    rejected_response(
        b"HTTP/1.1 101 Switching Protocols\r\n\r\n",
        ErrorKind::ConnectionRefused,
    )
    .await;
}

#[tokio::test]
async fn http_bounds_header_bytes_count_and_informational_responses() {
    rejected_response(&vec![b'a'; 16 * 1024], ErrorKind::InvalidData).await;
    let response = format!("HTTP/1.1 200 OK\r\n{}\r\n", "X: value\r\n".repeat(65));
    rejected_response(response.as_bytes(), ErrorKind::InvalidData).await;
    let response = "HTTP/1.1 100 Continue\r\n\r\n".repeat(5);
    rejected_response(response.as_bytes(), ErrorKind::InvalidData).await;

    // An exact-budget response succeeds; the first tunnel byte is untouched.
    timeout(DEADLINE, async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = proxied(
            "127.0.0.1:9".parse().unwrap(),
            listener.local_addr().unwrap(),
            "",
        );
        let server = async {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_request(&mut socket).await;
            let mut response = b"HTTP/1.1 200 OK\r\nX: ".to_vec();
            response.resize(16 * 1024 - 4, b'a');
            response.extend_from_slice(b"\r\n\r\nx");
            socket.write_all(&response).await.unwrap();
        };
        let client = async {
            let mut stream = NetStream::connect(&addr).await.unwrap();
            assert_eq!(stream.split().0.read_u8().await.unwrap(), b'x');
        };
        tokio::join!(client, server);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn http_setup_timeout_and_cancellation_close_the_proxy_socket() {
    timeout(DEADLINE, async {
        for cancel in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = proxied(
                "127.0.0.1:9".parse().unwrap(),
                listener.local_addr().unwrap(),
                "",
            );
            let (sent, received) = tokio::sync::oneshot::channel();
            let server = async {
                let (mut socket, _) = listener.accept().await.unwrap();
                read_request(&mut socket).await;
                // Stall in the middle of a response to exercise partial state.
                socket.write_all(b"HTTP/1.1 200 OK\r\nX: ").await.unwrap();
                sent.send(()).unwrap();
                let mut byte = [0];
                match socket.read(&mut byte).await {
                    Ok(0) => {}
                    Err(error) if error.kind() == ErrorKind::ConnectionReset => {}
                    result => panic!("cancelled setup did not close the socket: {result:?}"),
                }
            };
            let client = async {
                if cancel {
                    tokio::select! {
                        _ = NetStream::connect(&addr) => panic!("stalled setup completed"),
                        _ = received => {},
                    }
                } else {
                    assert!(
                        timeout(Duration::from_millis(30), NetStream::connect(&addr))
                            .await
                            .is_err()
                    );
                    received.await.unwrap();
                }
            };
            tokio::join!(client, server);
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn invalid_proxy_configuration_is_redacted_and_never_connects() {
    let target = "127.0.0.1:9".parse().unwrap();
    for url in [
        "http://synthetic:secret@127.0.0.1:invalid",
        "socks5://synthetic:secret@127.0.0.1",
        "ftp://synthetic:secret@nonexistent.invalid:443",
        "http://synthetic%3Auser:secret@127.0.0.1:9",
        "http://synthetic:se%0D%0Acret@127.0.0.1:9",
        "http://synthetic:%FF@127.0.0.1:9",
    ] {
        let addr = ServerAddr::Proxied {
            address: target,
            proxy: url.into(),
        };
        assert!(!format!("{addr:?}").contains("secret"));
        let Err(error) = NetStream::connect(&addr).await else {
            panic!("invalid proxy connected")
        };
        assert!(!format!("{error:?}").contains("secret"));
        assert!(!format!("{error:?}").contains("synthetic"));
    }
}

#[tokio::test]
async fn socks4_local_handshake_and_aliases() {
    timeout(DEADLINE, async {
        for (scheme, userinfo, expected_user) in [
            ("socks4", "", ""),
            ("socks4", "us%40er@", "us@er"),
            ("socks4a", "us%40er@", "us@er"),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = ServerAddr::Proxied {
                address: "192.0.2.1:443".parse().unwrap(),
                proxy: format!("{scheme}://{userinfo}{}", listener.local_addr().unwrap()),
            };
            let server = async {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 8];
                socket.read_exact(&mut request).await.unwrap();
                assert_eq!(&request, b"\x04\x01\x01\xbb\xc0\x00\x02\x01");
                let mut user = Vec::new();
                loop {
                    let byte = socket.read_u8().await.unwrap();
                    if byte == 0 {
                        break;
                    }
                    assert!(user.len() < 256);
                    user.push(byte);
                }
                assert_eq!(user, expected_user.as_bytes());
                socket
                    .write_all(b"\0\x5a\x01\xbb\x7f\0\0\x01tail")
                    .await
                    .unwrap();
            };
            let client = async {
                let mut stream = NetStream::connect(&addr).await.unwrap();
                let mut tail = [0; 4];
                stream.split().0.read_exact(&mut tail).await.unwrap();
                assert_eq!(&tail, b"tail");
            };
            tokio::join!(client, server);
        }
    })
    .await
    .unwrap();
    for (target, url) in [
        ("[::1]:443", "socks4://127.0.0.1:9"),
        ("127.0.0.1:443", "socks4://user:pass@127.0.0.1:9"),
        ("127.0.0.1:443", "socks4://us%00er@127.0.0.1:9"),
    ] {
        let addr = ServerAddr::Proxied {
            address: target.parse().unwrap(),
            proxy: url.into(),
        };
        let Err(error) = NetStream::connect(&addr).await else {
            panic!("invalid SOCKS4 accepted")
        };
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
    }
}

#[tokio::test]
async fn socks5_local_handshakes_still_support_anonymous_and_password_auth() {
    timeout(DEADLINE, async {
        for authenticated in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let userinfo = if authenticated { "us%40er:p%3Ass@" } else { "" };
            let addr = ServerAddr::Proxied {
                address: "192.0.2.1:443".parse().unwrap(),
                proxy: format!("socks5://{userinfo}{}", listener.local_addr().unwrap()),
            };
            let server = async {
                let (mut socket, _) = listener.accept().await.unwrap();
                assert_eq!(socket.read_u8().await.unwrap(), 5);
                let count = socket.read_u8().await.unwrap();
                let mut methods = vec![0; count as usize];
                socket.read_exact(&mut methods).await.unwrap();
                let method = if authenticated { 2 } else { 0 };
                assert!(methods.contains(&method));
                socket.write_all(&[5, method]).await.unwrap();
                if authenticated {
                    assert_eq!(socket.read_u8().await.unwrap(), 1);
                    for expected in [b"us@er".as_slice(), b"p:ss"] {
                        let len = socket.read_u8().await.unwrap();
                        let mut value = vec![0; len as usize];
                        socket.read_exact(&mut value).await.unwrap();
                        assert_eq!(value, expected);
                    }
                    socket.write_all(&[1, 0]).await.unwrap();
                }
                let mut request = [0; 10];
                socket.read_exact(&mut request).await.unwrap();
                assert_eq!(request, [5, 1, 0, 1, 192, 0, 2, 1, 1, 187]);
                socket
                    .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 80, b'x'])
                    .await
                    .unwrap();
                assert_eq!(socket.read_u8().await.unwrap(), b'y');
            };
            let client = async {
                let mut stream = NetStream::connect(&addr).await.unwrap();
                let NetStream::ProxySocks5(ref socket) = stream else {
                    panic!("expected SOCKS5")
                };
                assert!(socket.nodelay().unwrap());
                let (mut reader, mut writer) = stream.split();
                assert_eq!(reader.read_u8().await.unwrap(), b'x');
                writer.write_all(b"y").await.unwrap();
            };
            tokio::join!(client, server);
        }
    })
    .await
    .unwrap();
}
