// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#[allow(deprecated)] // see https://github.com/RustCrypto/block-ciphers/issues/509
use aes::cipher::{KeyIvInit, StreamCipher};

/// This implements the AES-256-CTR cipher used by Telegram to encrypt data
/// when using the obfuscated transport.
///
/// It is not intended to be used directly.
pub struct ObfuscatedCipher {
    rx: ctr::Ctr128BE<aes::Aes256>,
    tx: ctr::Ctr128BE<aes::Aes256>,
}

impl ObfuscatedCipher {
    pub fn new(init: &[u8; 64]) -> Self {
        let mut init_rev = *init;
        init_rev.reverse();
        #[allow(deprecated)] // see https://github.com/RustCrypto/block-ciphers/issues/509
        Self {
            rx: ctr::Ctr128BE::<aes::Aes256>::new(
                init_rev[8..40].try_into().unwrap(),
                init_rev[40..56].try_into().unwrap(),
            ),
            tx: ctr::Ctr128BE::<aes::Aes256>::new(
                init[8..40].try_into().unwrap(),
                init[40..56].try_into().unwrap(),
            ),
        }
    }

    /// Creates the secret-mixed AES-256-CTR streams used by an MTProxy.
    /// `secret` excludes its transport marker and any fake-TLS hostname.
    pub fn with_secret(init: &[u8; 64], secret: &[u8; 16]) -> Self {
        let mut init_rev = *init;
        init_rev.reverse();
        let rx_key = crate::sha256!(&init_rev[8..40], secret);
        let tx_key = crate::sha256!(&init[8..40], secret);
        #[allow(deprecated)] // see https://github.com/RustCrypto/block-ciphers/issues/509
        Self {
            rx: ctr::Ctr128BE::<aes::Aes256>::new(
                (&rx_key).into(),
                init_rev[40..56].try_into().unwrap(),
            ),
            tx: ctr::Ctr128BE::<aes::Aes256>::new(
                (&tx_key).into(),
                init[40..56].try_into().unwrap(),
            ),
        }
    }

    pub fn encrypt(&mut self, buffer: &mut [u8]) {
        self.tx.apply_keystream(buffer);
    }

    pub fn decrypt(&mut self, buffer: &mut [u8]) {
        self.rx.apply_keystream(buffer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mtproxy_secret_mixing_matches_independent_openssl_ctr_vectors() {
        // SHA-256(nonce[8..40] || [0x71;16]), nonce[40..56], AES-256-CTR.
        // Static ciphertext generated with Python hashlib + OpenSSL enc; the
        // reverse-nonce fixture independently checks the receive direction.
        let nonce = std::array::from_fn(|i| i as u8);
        let mut cipher = ObfuscatedCipher::with_secret(&nonce, &[0x71; 16]);
        let mut bytes: Vec<u8> = (0..64).collect();
        cipher.encrypt(&mut bytes[..13]);
        cipher.encrypt(&mut bytes[13..]);
        assert_eq!(
            crate::hex::to_hex(&bytes),
            "2b3dd2b9d658036114772ec925cbd516f18966547015f5fb19ce0525e8a2fc705791564bef7334e509a8060a386dbd97f49e85e0e48cf6122e4588325c7c34b1"
        );
        let mut bytes: Vec<u8> = (0..64).collect();
        cipher.decrypt(&mut bytes[..17]);
        cipher.decrypt(&mut bytes[17..]);
        assert_eq!(
            crate::hex::to_hex(&bytes),
            "dbbc2ec198284166905f24f4c6cf0e836feb8f189c27ac57a73fd1bed5ebe0473f697ba0dd7dd8c675f5c8e7e8b6dd257ecbd239b477c36b05d220d2b1a05d59"
        );
    }

    fn heap_scratch_cipher(init: &[u8; 64]) -> ObfuscatedCipher {
        let init_rev = init.iter().copied().rev().collect::<Vec<_>>();
        #[allow(deprecated)]
        ObfuscatedCipher {
            rx: ctr::Ctr128BE::<aes::Aes256>::new(
                init_rev[8..40].try_into().unwrap(),
                init_rev[40..56].try_into().unwrap(),
            ),
            tx: ctr::Ctr128BE::<aes::Aes256>::new(
                init[8..40].try_into().unwrap(),
                init[40..56].try_into().unwrap(),
            ),
        }
    }

    #[test]
    #[ignore = "isolated cipher setup benchmark; no network or per-packet work"]
    fn benchmark_obfuscated_cipher_setup() {
        use std::hint::black_box;
        const ITERATIONS: usize = 100_000;
        let init = std::array::from_fn(|i| i as u8);
        let mut samples = [Vec::new(), Vec::new()];
        for round in 0..8 {
            for optimized in [round % 2 == 0, round % 2 != 0] {
                let start = std::time::Instant::now();
                for _ in 0..ITERATIONS {
                    black_box(if optimized {
                        ObfuscatedCipher::new(black_box(&init))
                    } else {
                        heap_scratch_cipher(black_box(&init))
                    });
                }
                samples[usize::from(optimized)]
                    .push(start.elapsed().as_nanos() as f64 / ITERATIONS as f64);
            }
        }
        for sample in &mut samples {
            sample.sort_by(f64::total_cmp);
        }
        println!(
            "Obfuscated cipher setup median: heap scratch {:.1} ns, current {:.1} ns; baseline has one transient 64-byte Vec allocation per construction",
            (samples[0][3] + samples[0][4]) / 2.0,
            (samples[1][3] + samples[1][4]) / 2.0
        );
    }

    #[test]
    fn obfuscated_cipher_matches_independent_ctr_fixtures_across_chunks() {
        // Independently generated AES-256-CTR vectors (big-endian counter).
        let expected_tx = "061ea5a02f48e35029da73f780a033f9528ca6113b7f1043c185e07def9d0d5563784879ab3903ed2ccc7d6ed8fdcb44f55ad2454dd4213290cff18b223dfd5d72e8836b7cb423066af8daf651effc44f2f796fe8da258ac75cf4c475f14068c";
        let expected_rx = "2f9ce173dac900479dd03be8062b7566c12ca4aef1259661f55cedaa051c6c714468fecb1d1db73daf32c727035c770b6210035cb9bbb5626cc89b70bb713adfd069dbc91bed7db91e29fd0f8ca949e0f7b9b71b101ff83567e9dba94649f226";
        let init = std::array::from_fn(|i| i as u8);
        let mut cipher = ObfuscatedCipher::new(&init);
        let mut tx: Vec<u8> = (0..96).collect();
        let mut rx = tx.clone();
        cipher.encrypt(&mut tx[..64]);
        cipher.encrypt(&mut tx[64..]);
        for chunk in rx.chunks_mut(13) {
            cipher.decrypt(chunk);
        }
        let from_hex = |hex: &str| {
            hex.as_bytes()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(tx, from_hex(expected_tx));
        assert_eq!(rx, from_hex(expected_rx));
    }
}
