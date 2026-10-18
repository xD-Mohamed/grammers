// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use grammers_crypto::{DequeBuffer, ObfuscatedCipher};

use super::{Error, Tagged, Transport, UnpackedOffset};

/// Obfuscation layer for other MTProto transport protocols.
///
/// This was made by Telegram to avoid ISP blocks.
/// This is needed to connect to the Telegram servers
/// using websockets or when conecting to MTProto proxies.
///
/// This wrapper encrypts the data using AES-256-CTR with a randomly
/// generated key that is then sent at the beginning of the connection.
///
/// Obfuscated transport can only be used with "tagged" transports, which
/// provide a way to get the obfuscated tag that is used in the encryption.
///
/// See [Transport Obfuscation](https://core.telegram.org/mtproto/mtproto-transports#transport-obfuscation)
/// for more information.
pub struct Obfuscated<T: Transport + Tagged> {
    inner: T,
    head: Option<[u8; 64]>,
    decrypt_tail: usize,
    cipher: ObfuscatedCipher,
}

const FORBIDDEN_FIRST_INTS: [[u8; 4]; 7] = [
    *b"HEAD",                 // HTTP HEAD
    *b"POST",                 // HTTP POST
    *b"GET ",                 // HTTP GET
    *b"OPTI",                 // HTTP OPTIONS
    [0x16, 0x03, 0x01, 0x02], // TLS handshake
    [0xdd, 0xdd, 0xdd, 0xdd], // Padded Intermediate
    [0xee, 0xee, 0xee, 0xee], // Intermediate
];

impl<T: Transport + Tagged> Obfuscated<T> {
    fn generate_keys(inner: &mut T) -> ([u8; 64], ObfuscatedCipher) {
        let mut init = [0; 64];

        while init[4..8] == [0; 4] // Full
            || init[0] == 0xef // Abridged
            || FORBIDDEN_FIRST_INTS.iter().any(|start| start == &init[..4])
        {
            getrandom::fill(&mut init).unwrap();
        }

        init[56..60].copy_from_slice(&inner.init_tag());

        let mut cipher = ObfuscatedCipher::new(&init);

        let mut encrypted_init = init;
        cipher.encrypt(&mut encrypted_init);
        init[56..64].copy_from_slice(&encrypted_init[56..64]);

        (init, cipher)
    }

    pub fn new(mut inner: T) -> Self {
        let (init, cipher) = Self::generate_keys(&mut inner);

        Self {
            inner,
            head: Some(init),
            decrypt_tail: 0,
            cipher,
        }
    }
}

impl<T: Transport + Tagged> Transport for Obfuscated<T> {
    fn pack(&mut self, buffer: &mut DequeBuffer<u8>) {
        self.inner.pack(buffer);
        self.cipher.encrypt(buffer.as_mut());
        if let Some(head) = self.head.take() {
            buffer.extend_front(&head);
        }
    }

    fn unpack(&mut self, buffer: &mut [u8]) -> Result<UnpackedOffset, Error> {
        if buffer.len() < self.decrypt_tail {
            panic!("buffer is smaller than what was decrypted");
        }

        self.cipher.decrypt(&mut buffer[self.decrypt_tail..]);
        self.decrypt_tail = buffer.len();

        match self.inner.unpack(buffer) {
            Ok(offset) => {
                self.decrypt_tail -= offset.next_offset;
                Ok(offset)
            }
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::Abridged;

    fn heap_scratch_handshake(inner: &mut Abridged) -> ([u8; 64], ObfuscatedCipher) {
        let mut init = [0; 64];
        while init[4..8] == [0; 4]
            || init[0] == 0xef
            || FORBIDDEN_FIRST_INTS.iter().any(|start| start == &init[..4])
        {
            getrandom::fill(&mut init).unwrap();
        }
        init[56..60].copy_from_slice(&inner.init_tag());
        let mut cipher = ObfuscatedCipher::new(&init);
        let mut encrypted_init = init.to_vec();
        cipher.encrypt(&mut encrypted_init);
        init[56..64].copy_from_slice(&encrypted_init[56..64]);
        (init, cipher)
    }

    #[test]
    #[ignore = "isolated obfuscated handshake setup benchmark; randomness included, no network"]
    fn benchmark_obfuscated_handshake_setup() {
        use std::hint::black_box;
        const ITERATIONS: usize = 25_000;
        let mut samples = [Vec::new(), Vec::new()];
        for round in 0..8 {
            for optimized in [round % 2 == 0, round % 2 != 0] {
                let start = std::time::Instant::now();
                for _ in 0..ITERATIONS {
                    let mut inner = Abridged::new();
                    black_box(if optimized {
                        Obfuscated::generate_keys(black_box(&mut inner))
                    } else {
                        heap_scratch_handshake(black_box(&mut inner))
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
            "Obfuscated handshake setup median: heap scratch {:.1} ns, current {:.1} ns; baseline adds one transient 64-byte Vec allocation; both use current cipher initialization",
            (samples[0][3] + samples[0][4]) / 2.0,
            (samples[1][3] + samples[1][4]) / 2.0
        );
    }

    #[test]
    fn obfuscated_handshake_preserves_tag_and_advances_outgoing_cipher_once() {
        let mut transport = Obfuscated::new(Abridged::new());
        let head = transport.head.unwrap();
        let mut reverse_head = head;
        reverse_head.reverse();
        let mut peer = ObfuscatedCipher::new(&reverse_head);
        let mut decoded_head = head;
        peer.decrypt(&mut decoded_head);
        assert_eq!(&decoded_head[56..60], &[0xef; 4]);

        for first in [true, false] {
            let payload = [1, 2, 3, 4, 5, 6, 7, 8];
            let mut buffer = DequeBuffer::with_capacity(128, 65);
            buffer.extend(payload);
            transport.pack(&mut buffer);
            let mut encrypted = buffer.as_ref().to_vec();
            if first {
                assert_eq!(&encrypted[..64], &head);
                encrypted.drain(..64);
            }
            peer.decrypt(&mut encrypted);
            assert_eq!(encrypted, [2, 1, 2, 3, 4, 5, 6, 7, 8]);
        }
    }
}
