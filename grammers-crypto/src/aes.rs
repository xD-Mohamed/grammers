// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! [AES-Infinite Garble Extension](https://mgp25.com/blog/2015/06/21/AESIGE/) implementation.

#![allow(deprecated)] // see https://github.com/RustCrypto/block-ciphers/issues/509

use aes::Aes256;
use aes::cipher::consts::U16;
use aes::cipher::{
    Block, BlockCipherDecBackend, BlockCipherDecClosure, BlockCipherDecrypt, BlockCipherEncBackend,
    BlockCipherEncClosure, BlockCipherEncrypt, BlockSizeUser, KeyInit,
};

/// One IGE pass over a buffer, run inside the cipher's backend closure: the
/// CPU-feature dispatch happens once per buffer instead of once per block, and
/// the chaining values stay in registers. Every block depends on the previous
/// one, so the mode itself cannot process blocks in parallel.
struct Ige<'a> {
    buffer: &'a mut [u8],
    iv: &'a [u8; 32],
}

impl BlockSizeUser for Ige<'_> {
    type BlockSize = U16;
}

impl Ige<'_> {
    /// The previous ciphertext (`iv1`) and plaintext (`iv2`) blocks.
    fn chaining_values(&self) -> (u128, u128) {
        let (iv1, iv2) = self.iv.split_at(16);
        (
            u128::from_ne_bytes(iv1.try_into().unwrap()),
            u128::from_ne_bytes(iv2.try_into().unwrap()),
        )
    }
}

impl BlockCipherEncClosure for Ige<'_> {
    #[inline(always)]
    fn call<B: BlockCipherEncBackend<BlockSize = U16>>(self, backend: &B) {
        let (mut iv1, mut iv2) = self.chaining_values();
        for chunk in self.buffer.as_chunks_mut::<16>().0 {
            let plaintext = u128::from_ne_bytes(*chunk);
            let mut block = Block::<Self>::from((plaintext ^ iv1).to_ne_bytes());
            backend.encrypt_block_inplace(&mut block);
            let ciphertext = u128::from_ne_bytes(block.into()) ^ iv2;
            *chunk = ciphertext.to_ne_bytes();
            iv1 = ciphertext;
            iv2 = plaintext;
        }
    }
}

impl BlockCipherDecClosure for Ige<'_> {
    #[inline(always)]
    fn call<B: BlockCipherDecBackend<BlockSize = U16>>(self, backend: &B) {
        let (mut iv1, mut iv2) = self.chaining_values();
        for chunk in self.buffer.as_chunks_mut::<16>().0 {
            let ciphertext = u128::from_ne_bytes(*chunk);
            let mut block = Block::<Self>::from((ciphertext ^ iv2).to_ne_bytes());
            backend.decrypt_block_inplace(&mut block);
            let plaintext = u128::from_ne_bytes(block.into()) ^ iv1;
            *chunk = plaintext.to_ne_bytes();
            iv1 = ciphertext;
            iv2 = plaintext;
        }
    }
}

/// Encrypt the input plaintext in-place using the AES-IGE mode.
/// The buffer length must be a multiple of 16.
pub fn ige_encrypt(buffer: &mut [u8], key: &[u8; 32], iv: &[u8; 32]) {
    assert_eq!(buffer.len() % 16, 0);
    Aes256::new(key.into()).encrypt_with_backend(Ige { buffer, iv });
}

/// Decrypt the input ciphertext in-place using the AES-IGE mode.
/// The buffer length must be a multiple of 16.
pub fn ige_decrypt(buffer: &mut [u8], key: &[u8; 32], iv: &[u8; 32]) {
    assert_eq!(buffer.len() % 16, 0);
    Aes256::new(key.into()).decrypt_with_backend(Ige { buffer, iv });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem;

    /// The previous block-at-a-time implementation, kept as a reference.
    fn reference_encrypt(buffer: &mut [u8], key: &[u8; 32], iv: &[u8; 32]) {
        let cipher = Aes256::new(key.into());
        let mut iv1: [u8; 16] = iv[0..16].try_into().unwrap();
        let mut iv2: [u8; 16] = iv[16..32].try_into().unwrap();
        let mut next_iv2 = [0u8; 16];
        for block in buffer.chunks_mut(16) {
            next_iv2.copy_from_slice(block);
            for i in 0..16 {
                block[i] ^= iv1[i]
            }
            cipher.encrypt_block(block.try_into().unwrap());
            for i in 0..16 {
                block[i] ^= iv2[i]
            }
            iv1.copy_from_slice(block);
            mem::swap(&mut iv2, &mut next_iv2);
        }
    }

    fn reference_decrypt(buffer: &mut [u8], key: &[u8; 32], iv: &[u8; 32]) {
        let cipher = Aes256::new(key.into());
        let mut iv1: [u8; 16] = iv[0..16].try_into().unwrap();
        let mut iv2: [u8; 16] = iv[16..32].try_into().unwrap();
        let mut next_iv1 = [0u8; 16];
        for block in buffer.chunks_mut(16) {
            next_iv1.copy_from_slice(block);
            for i in 0..16 {
                block[i] ^= iv2[i]
            }
            cipher.decrypt_block(block.try_into().unwrap());
            for i in 0..16 {
                block[i] ^= iv1[i]
            }
            mem::swap(&mut iv1, &mut next_iv1);
            iv2.copy_from_slice(block);
        }
    }

    /// Deterministic pseudorandom bytes (xorshift), no test dependency.
    fn bytes(seed: u64, len: usize) -> Vec<u8> {
        let mut state = seed | 1;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect()
    }

    #[test]
    fn matches_the_block_at_a_time_reference_and_round_trips() {
        for (case, len) in [0, 16, 32, 48, 448, 1024, 2096, 16 * 1024]
            .into_iter()
            .enumerate()
        {
            let key: [u8; 32] = bytes(case as u64 * 3 + 1, 32).try_into().unwrap();
            let iv: [u8; 32] = bytes(case as u64 * 3 + 2, 32).try_into().unwrap();
            let plaintext = bytes(case as u64 * 3 + 3, len);

            let mut ours = plaintext.clone();
            let mut reference = plaintext.clone();
            ige_encrypt(&mut ours, &key, &iv);
            reference_encrypt(&mut reference, &key, &iv);
            assert_eq!(ours, reference, "encrypt {len}");

            let mut decrypted = ours.clone();
            reference_decrypt(&mut reference, &key, &iv);
            ige_decrypt(&mut decrypted, &key, &iv);
            assert_eq!(decrypted, plaintext, "decrypt {len}");
            assert_eq!(reference, plaintext, "reference decrypt {len}");
        }
    }

    #[test]
    #[should_panic]
    fn rejects_partial_blocks() {
        ige_encrypt(&mut [0; 15], &[0; 32], &[0; 32]);
    }

    #[test]
    #[ignore = "local IGE implementation comparison; run release with nocapture"]
    fn benchmark_ige_backend_closure() {
        use std::hint::black_box;
        use std::time::Instant;
        let key = [3u8; 32];
        let iv = [5u8; 32];
        for len in [448usize, 656, 2096] {
            let mut buffer = bytes(len as u64, len);
            let iterations = 2_000_000 / len;
            let mut samples = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
            for round in 0..8 {
                for k in 0..4 {
                    let k = (k + round) % 4;
                    let began = Instant::now();
                    for _ in 0..iterations {
                        match k {
                            0 => reference_encrypt(black_box(&mut buffer), &key, &iv),
                            1 => ige_encrypt(black_box(&mut buffer), &key, &iv),
                            2 => reference_decrypt(black_box(&mut buffer), &key, &iv),
                            _ => ige_decrypt(black_box(&mut buffer), &key, &iv),
                        }
                    }
                    samples[k].push(began.elapsed().as_nanos() as f64 / iterations as f64);
                }
            }
            for sample in &mut samples {
                sample.sort_by(f64::total_cmp);
            }
            println!(
                "{len} B: encrypt per-block {:.1} ns, closure {:.1} ns; decrypt per-block {:.1} ns, closure {:.1} ns",
                samples[0][4], samples[1][4], samples[2][4], samples[3][4]
            );
        }
    }
}
