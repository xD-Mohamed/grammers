//! Print an offline native-format demonstration, never an account credential.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};

use grammers_session::storages::MemorySession;
use grammers_session::types::DcOption;
use grammers_session::{NativeSession, SessionData};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Deliberately synthetic bytes and RFC documentation addresses. These have
    // never been used for Telegram authorization. No network or files are read.
    let key = std::array::from_fn(|index| (index as u8).wrapping_mul(73).wrapping_add(17));
    let source = MemorySession::from(SessionData {
        home_dc: 2,
        dc_options: HashMap::from([(
            2,
            DcOption {
                id: 2,
                ipv4: SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 1), 443),
                ipv6: SocketAddrV6::new(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 2), 443, 0, 0),
                auth_key: Some(key),
            },
        )]),
        ..Default::default()
    });
    let text = NativeSession::from_session(&source, false)?.encode();
    let decoded = NativeSession::parse(text.expose_secret())?;
    if decoded.encode().expose_secret() != text.expose_secret() {
        return Err("native session demonstration did not round-trip".into());
    }
    println!("Synthetic demonstration only, not a Telegram login credential.");
    println!("{}", text.expose_secret());
    println!("Length: {} ASCII characters", text.expose_secret().len());
    Ok(())
}
