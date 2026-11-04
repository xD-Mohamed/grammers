// Copyright 2026 - developers of the `grammers` project.
// Licensed under the Apache License, Version 2.0 or the MIT license, at your option.

use std::{
    future::poll_fn,
    io,
    pin::Pin,
    sync::{Arc, OnceLock},
    task::{Context, Poll, ready},
};
use tokio::{
    io::{AsyncWrite, AsyncWriteExt},
    net::TcpStream,
};
use tokio_rustls::{
    TlsConnector,
    rustls::{ClientConfig, RootCertStore, pki_types::ServerName},
};

pub(super) type TlsStream = tokio_rustls::client::TlsStream<TcpStream>;

fn client_config(roots: RootCertStore) -> io::Result<ClientConfig> {
    // Select a backend for this client without changing the application's
    // process-wide crypto provider.
    ClientConfig::builder_with_provider(Arc::new(
        tokio_rustls::rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map(|builder| builder.with_root_certificates(roots).with_no_client_auth())
    .map_err(io::Error::other)
}

fn default_config() -> io::Result<Arc<ClientConfig>> {
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    if let Some(config) = CONFIG.get() {
        return Ok(Arc::clone(config));
    }
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = Arc::new(client_config(roots)?);
    Ok(Arc::clone(CONFIG.get_or_init(|| config)))
}

pub(super) async fn connect(stream: TcpStream, hostname: &str) -> io::Result<TlsStream> {
    connect_with_config(stream, hostname, default_config()?).await
}

// Tests may inject a synthetic CA, never an insecure verifier.
pub(super) async fn connect_with_config(
    stream: TcpStream,
    hostname: &str,
    config: Arc<ClientConfig>,
) -> io::Result<TlsStream> {
    let hostname = hostname
        .strip_prefix('[')
        .and_then(|name| name.strip_suffix(']'))
        .unwrap_or(hostname);
    let server_name = ServerName::try_from(hostname.to_owned()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid HTTPS proxy server name",
        )
    })?;
    TlsConnector::from(config)
        .connect(server_name, stream)
        .await
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("HTTPS proxy TLS handshake failed: {error}"),
            )
        })
}

#[cfg(test)]
fn test_config(
    certificate: tokio_rustls::rustls::pki_types::CertificateDer<'static>,
) -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots.add(certificate).unwrap();
    Arc::new(client_config(roots).unwrap())
}

/// Flush accepted carrier plaintext before the sender retires its write prefix.
/// The count survives cancellation of a select branch while flushing. The
/// sender retains the same serialized prefix until this operation returns.
/// An inherent operation avoids violating AsyncWrite's Pending contract.
pub(crate) struct FlushWriter<W> {
    inner: W,
    pending: Option<usize>,
}
impl<W: AsyncWrite + Unpin> FlushWriter<W> {
    pub(super) fn new(inner: W) -> Self {
        Self {
            inner,
            pending: None,
        }
    }
    pub(super) async fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        poll_fn(|cx| self.poll_write_flushed(cx, bytes)).await
    }
    fn poll_write_flushed(
        &mut self,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let accepted = match self.pending {
            Some(count) => count,
            None => {
                let count = ready!(Pin::new(&mut self.inner).poll_write(cx, bytes))?;
                self.pending = Some(count);
                count
            }
        };
        ready!(Pin::new(&mut self.inner).poll_flush(cx))?;
        self.pending = None;
        Poll::Ready(Ok(accepted))
    }
    pub(super) async fn shutdown(&mut self) -> io::Result<()> {
        self.inner.shutdown().await
    }
    #[cfg(test)]
    async fn write_all(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let count = self.write(bytes).await?;
            if count == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            bytes = &bytes[count..];
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{future::Future, time::Duration};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        time::timeout,
    };
    use tokio_rustls::{
        TlsAcceptor,
        rustls::{ServerConfig, pki_types::PrivatePkcs8KeyDer},
    };
    const DEADLINE: Duration = Duration::from_secs(5);

    fn configs() -> (Arc<ClientConfig>, TlsAcceptor) {
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let certificate = certified.cert.der().clone();
        let key = PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der());
        let server = ServerConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![certificate.clone()], key.into())
        .unwrap();
        (
            test_config(certificate),
            TlsAcceptor::from(Arc::new(server)),
        )
    }

    #[tokio::test]
    async fn verified_https_connect_preserves_tunnel_bytes_and_flushes_writes() {
        timeout(DEADLINE, async {
            let (config, acceptor) = configs();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = async {
                let (tcp, _) = listener.accept().await.unwrap();
                let mut tls = acceptor.accept(tcp).await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") { request.push(tls.read_u8().await.unwrap()); }
                assert_eq!(request, b"CONNECT 192.0.2.1:443 HTTP/1.1\r\nHost: 192.0.2.1:443\r\nProxy-Authorization: Basic dXNlcjpwYXNz\r\n\r\n");
                tls.write_all(b"HTTP/1.1 200 OK\r\n\r\nreply").await.unwrap();
                tls.flush().await.unwrap();
                let mut message = [0; 7];
                tls.read_exact(&mut message).await.unwrap();
                assert_eq!(&message, b"request");
                assert_eq!(tls.read(&mut [0; 1]).await.unwrap(), 0);
                tls.shutdown().await.unwrap();
            };
            let client = async {
                let tcp = TcpStream::connect(address).await.unwrap();
                let tls = connect_with_config(tcp, "localhost", config).await.unwrap();
                let mut buffered = tokio::io::BufReader::with_capacity(1024, tls);
                let request = super::super::http_connect::request(&"192.0.2.1:443".parse().unwrap(), "user", "pass", true).unwrap();
                super::super::http_connect::handshake_buffered(&mut buffered, &request).await.unwrap();
                let (mut reader, writer) = tokio::io::split(buffered);
                let mut writer = FlushWriter::new(writer);
                let mut reply = [0; 5];
                reader.read_exact(&mut reply).await.unwrap();
                assert_eq!(&reply, b"reply");
                writer.write_all(b"request").await.unwrap();
                writer.shutdown().await.unwrap();
            };
            tokio::join!(client, server);
        }).await.unwrap();
    }

    #[tokio::test]
    async fn tls_rejects_untrusted_certificate_and_wrong_server_name() {
        timeout(DEADLINE, async {
            for trust in [false, true] {
                let (config, acceptor) = configs();
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = listener.local_addr().unwrap();
                let server = async {
                    let (tcp, _) = listener.accept().await.unwrap();
                    assert!(acceptor.accept(tcp).await.is_err());
                };
                let client = async {
                    let tcp = TcpStream::connect(address).await.unwrap();
                    let result = if trust {
                        connect_with_config(tcp, "wrong.invalid", config).await
                    } else {
                        connect(tcp, "localhost").await
                    };
                    assert!(
                        result
                            .unwrap_err()
                            .to_string()
                            .contains("HTTPS proxy TLS handshake failed")
                    );
                };
                tokio::join!(client, server);
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn cancelling_tls_handshake_closes_the_socket() {
        timeout(DEADLINE, async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let tcp = TcpStream::connect(listener.local_addr().unwrap())
                .await
                .unwrap();
            let (mut server, _) = listener.accept().await.unwrap();
            let mut handshake = Box::pin(connect(tcp, "localhost"));
            poll_fn(|cx| {
                assert!(handshake.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            drop(handshake);
            let mut hello = Vec::new();
            server.read_to_end(&mut hello).await.unwrap();
            assert!(!hello.is_empty());
        })
        .await
        .unwrap();
    }

    struct BackpressuredWriter {
        bytes: Vec<u8>,
        flush_ready: bool,
        write_limit: usize,
    }
    impl AsyncWrite for BackpressuredWriter {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            let count = bytes.len().min(self.write_limit);
            self.bytes.extend_from_slice(&bytes[..count]);
            Poll::Ready(Ok(count))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            if self.flush_ready {
                Poll::Ready(Ok(()))
            } else {
                Poll::Pending
            }
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }
    #[tokio::test]
    async fn cancelling_flush_retains_partial_write_and_never_replays_it() {
        let mut writer = FlushWriter::new(BackpressuredWriter {
            bytes: Vec::new(),
            flush_ready: false,
            write_limit: 3,
        });
        for _ in 0..3 {
            let mut write = std::pin::pin!(writer.write(b"request"));
            poll_fn(|cx| {
                assert!(write.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
        }
        assert_eq!(writer.inner.bytes, b"req");
        writer.inner.flush_ready = true;
        assert_eq!(writer.write(b"request").await.unwrap(), 3);
        writer.write_all(b"uest").await.unwrap();
        assert_eq!(writer.inner.bytes, b"request");
    }
}
