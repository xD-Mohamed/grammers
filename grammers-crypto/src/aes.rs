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
/// CPU-feature dispatch happens once per buffer instead of once per block.
/// Every block depends on the previous one, so the mode itself cannot process
/// blocks in parallel. This is the portable path; see [`x86`] for AES-NI.
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

/// AES-NI applied directly to the IGE chain.
///
/// IGE is one serial dependency chain: every block waits for the previous one, so
/// its speed is the latency of that chain and nothing else. The portable closure
/// above goes through the cipher's block interface, which moves every block between
/// vector and general registers on that chain. Here a block stays in one vector
/// register from load to store, and the two whitening XORs that do not depend on
/// the previous block are folded into the first and last round keys, leaving the
/// fourteen rounds and one XOR per block. Output is byte-identical; the tests
/// compare both paths.
#[cfg(target_arch = "x86_64")]
mod x86 {
    use std::arch::x86_64::*;

    /// Whether this processor has the instructions used below. The standard
    /// library caches the answer, so asking per message costs a load.
    #[inline]
    pub(super) fn available() -> bool {
        std::arch::is_x86_feature_detected!("aes") && std::arch::is_x86_feature_detected!("sse4.1")
    }

    #[inline]
    #[target_feature(enable = "aes,sse4.1")]
    fn load(bytes: &[u8; 16]) -> __m128i {
        let (low, high) = bytes.split_at(8);
        _mm_set_epi64x(
            i64::from_le_bytes(high.try_into().unwrap()),
            i64::from_le_bytes(low.try_into().unwrap()),
        )
    }

    #[inline]
    #[target_feature(enable = "aes,sse4.1")]
    fn store(bytes: &mut [u8; 16], value: __m128i) {
        let (low, high) = bytes.split_at_mut(8);
        low.copy_from_slice(&_mm_cvtsi128_si64(value).to_le_bytes());
        high.copy_from_slice(&_mm_extract_epi64(value, 1).to_le_bytes());
    }

    /// The AES-256 encryption round keys (FIPS 197, section 5.2).
    #[target_feature(enable = "aes,sse4.1")]
    fn expand(key: &[u8; 32]) -> [__m128i; 15] {
        // w[i] = w[i - 8] ^ w[i - 1] for each of the four words, as three shifts.
        #[inline]
        #[target_feature(enable = "aes,sse4.1")]
        fn mix(previous: __m128i, assist: __m128i) -> __m128i {
            let mut key = previous;
            key = _mm_xor_si128(key, _mm_slli_si128(key, 4));
            key = _mm_xor_si128(key, _mm_slli_si128(key, 4));
            key = _mm_xor_si128(key, _mm_slli_si128(key, 4));
            _mm_xor_si128(key, assist)
        }
        macro_rules! even {
            ($keys:ident, $i:expr, $rcon:expr) => {
                $keys[$i] = mix(
                    $keys[$i - 2],
                    _mm_shuffle_epi32(_mm_aeskeygenassist_si128($keys[$i - 1], $rcon), 0xff),
                );
            };
        }
        macro_rules! odd {
            ($keys:ident, $i:expr) => {
                $keys[$i] = mix(
                    $keys[$i - 2],
                    _mm_shuffle_epi32(_mm_aeskeygenassist_si128($keys[$i - 1], 0), 0xaa),
                );
            };
        }
        let (first, second) = key.split_at(16);
        let mut keys = [_mm_setzero_si128(); 15];
        keys[0] = load(first.try_into().unwrap());
        keys[1] = load(second.try_into().unwrap());
        even!(keys, 2, 0x01);
        odd!(keys, 3);
        even!(keys, 4, 0x02);
        odd!(keys, 5);
        even!(keys, 6, 0x04);
        odd!(keys, 7);
        even!(keys, 8, 0x08);
        odd!(keys, 9);
        even!(keys, 10, 0x10);
        odd!(keys, 11);
        even!(keys, 12, 0x20);
        odd!(keys, 13);
        even!(keys, 14, 0x40);
        keys
    }

    #[target_feature(enable = "aes,sse4.1")]
    pub(super) fn encrypt(buffer: &mut [u8], key: &[u8; 32], iv: &[u8; 32]) {
        let keys = expand(key);
        let (iv1, iv2) = iv.split_at(16);
        // The previous ciphertext and plaintext blocks.
        let mut iv1 = load(iv1.try_into().unwrap());
        let mut iv2 = load(iv2.try_into().unwrap());
        for chunk in buffer.as_chunks_mut::<16>().0 {
            let plaintext = load(chunk);
            // c = E(p ^ iv1) ^ iv2, with p ^ k0 and k14 ^ iv2 off the chain.
            let mut block = _mm_xor_si128(_mm_xor_si128(plaintext, keys[0]), iv1);
            for key in &keys[1..14] {
                block = _mm_aesenc_si128(block, *key);
            }
            let ciphertext = _mm_aesenclast_si128(block, _mm_xor_si128(keys[14], iv2));
            store(chunk, ciphertext);
            iv1 = ciphertext;
            iv2 = plaintext;
        }
    }

    /// The round keys of the equivalent inverse cipher (FIPS 197, section 5.3.5):
    /// the same ones in reverse, the inner ones through InvMixColumns.
    #[target_feature(enable = "aes,sse4.1")]
    fn expand_inverse(key: &[u8; 32]) -> [__m128i; 15] {
        let encryption = expand(key);
        let mut keys = [_mm_setzero_si128(); 15];
        keys[0] = encryption[14];
        for round in 1..14 {
            keys[round] = _mm_aesimc_si128(encryption[14 - round]);
        }
        keys[14] = encryption[0];
        keys
    }

    /// The previous ciphertext and plaintext blocks.
    #[target_feature(enable = "aes,sse4.1")]
    fn chaining_values(iv: &[u8; 32]) -> (__m128i, __m128i) {
        let (iv1, iv2) = iv.split_at(16);
        (load(iv1.try_into().unwrap()), load(iv2.try_into().unwrap()))
    }

    /// Decrypt whole blocks of `data`, continuing the chain in `iv1` and `iv2`.
    #[inline]
    #[target_feature(enable = "aes,sse4.1")]
    fn decrypt_blocks(keys: &[__m128i; 15], iv1: &mut __m128i, iv2: &mut __m128i, data: &mut [u8]) {
        for chunk in data.as_chunks_mut::<16>().0 {
            let ciphertext = load(chunk);
            // p = D(c ^ iv2) ^ iv1, with c ^ k0 and k14 ^ iv1 off the chain.
            let mut block = _mm_xor_si128(_mm_xor_si128(ciphertext, keys[0]), *iv2);
            for key in &keys[1..14] {
                block = _mm_aesdec_si128(block, *key);
            }
            let plaintext = _mm_aesdeclast_si128(block, _mm_xor_si128(keys[14], *iv1));
            store(chunk, plaintext);
            *iv1 = ciphertext;
            *iv2 = plaintext;
        }
    }

    #[target_feature(enable = "aes,sse4.1")]
    pub(super) fn decrypt(buffer: &mut [u8], key: &[u8; 32], iv: &[u8; 32]) {
        let keys = expand_inverse(key);
        let (mut iv1, mut iv2) = chaining_values(iv);
        decrypt_blocks(&keys, &mut iv1, &mut iv2, buffer);
    }

    /// Decrypt as [`decrypt`] and return the SHA-256 of `prefix` followed by the
    /// plaintext, which is how an incoming message is authenticated.
    ///
    /// Decryption is one serial chain and so is hashing, and neither needs the
    /// other's result of the same 64 bytes: the hash runs one block behind. Issued
    /// together, the processor works on both at once, and the hash that used to
    /// follow as a second pass over the plaintext costs little more than nothing.
    #[target_feature(enable = "aes,sse4.1")]
    pub(super) fn decrypt_hashed(
        buffer: &mut [u8],
        key: &[u8; 32],
        iv: &[u8; 32],
        prefix: &[u8; 32],
    ) -> [u8; 32] {
        use sha2::block_api::compress256;

        let keys = expand_inverse(key);
        let (mut iv1, mut iv2) = chaining_values(iv);
        // SHA-256's initial hash value (FIPS 180-4, section 5.3.3).
        let mut state: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
            0x5be0cd19,
        ];
        // The prefix is half a hash block, so hash blocks straddle the 64-byte
        // steps taken here: all but the first lie whole in the plaintext.
        let len = buffer.len();
        let steps = len / 64;
        let mut hashed = None;
        for step in 0..steps {
            decrypt_blocks(
                &keys,
                &mut iv1,
                &mut iv2,
                &mut buffer[step * 64..step * 64 + 64],
            );
            // A block that earlier steps finished decrypting.
            match hashed {
                None if step == 1 => {
                    let mut first = [0; 64];
                    first[..32].copy_from_slice(prefix);
                    first[32..].copy_from_slice(&buffer[..32]);
                    compress256(&mut state, &[first]);
                    hashed = Some(32);
                }
                None => {}
                Some(done) => {
                    let block: &[u8; 64] = buffer[done..done + 64].try_into().unwrap();
                    compress256(&mut state, std::slice::from_ref(block));
                    hashed = Some(done + 64);
                }
            }
        }
        decrypt_blocks(&keys, &mut iv1, &mut iv2, &mut buffer[steps * 64..]);

        // What is left: at most 144 bytes (the prefix too, if there were fewer than
        // two steps), then the padding of FIPS 180-4, section 5.1.1.
        let mut tail = [0; 192];
        let mut filled = 0;
        if hashed.is_none() {
            tail[..32].copy_from_slice(prefix);
            filled = 32;
        }
        let rest = &buffer[hashed.unwrap_or(0)..];
        tail[filled..filled + rest.len()].copy_from_slice(rest);
        filled += rest.len();
        tail[filled] = 0x80;
        let blocks = (filled + 1 + 8).div_ceil(64);
        let bits = (prefix.len() as u64 + len as u64) * 8;
        tail[blocks * 64 - 8..blocks * 64].copy_from_slice(&bits.to_be_bytes());
        compress256(&mut state, tail[..blocks * 64].as_chunks::<64>().0);

        let mut digest = [0; 32];
        for (bytes, word) in digest.as_chunks_mut::<4>().0.iter_mut().zip(state) {
            *bytes = word.to_be_bytes();
        }
        digest
    }
}

/// The cipher crate's own backend, whichever it selects: every other target, and
/// x86-64 processors without AES-NI.
fn portable_encrypt(buffer: &mut [u8], key: &[u8; 32], iv: &[u8; 32]) {
    Aes256::new(key.into()).encrypt_with_backend(Ige { buffer, iv });
}

fn portable_decrypt(buffer: &mut [u8], key: &[u8; 32], iv: &[u8; 32]) {
    Aes256::new(key.into()).decrypt_with_backend(Ige { buffer, iv });
}

/// Encrypt the input plaintext in-place using the AES-IGE mode.
/// The buffer length must be a multiple of 16.
pub fn ige_encrypt(buffer: &mut [u8], key: &[u8; 32], iv: &[u8; 32]) {
    assert_eq!(buffer.len() % 16, 0);
    #[cfg(target_arch = "x86_64")]
    if x86::available() {
        // SAFETY: `available` just confirmed the processor supports every target
        // feature `x86::encrypt` is compiled with. It takes and returns only safe
        // Rust types and contains no other unsafe operation.
        #[allow(unsafe_code)]
        unsafe {
            x86::encrypt(buffer, key, iv)
        };
        return;
    }
    portable_encrypt(buffer, key, iv);
}

/// Decrypt the input ciphertext in-place using the AES-IGE mode.
/// The buffer length must be a multiple of 16.
pub fn ige_decrypt(buffer: &mut [u8], key: &[u8; 32], iv: &[u8; 32]) {
    assert_eq!(buffer.len() % 16, 0);
    #[cfg(target_arch = "x86_64")]
    if x86::available() {
        // SAFETY: as in `ige_encrypt`.
        #[allow(unsafe_code)]
        unsafe {
            x86::decrypt(buffer, key, iv)
        };
        return;
    }
    portable_decrypt(buffer, key, iv);
}

/// Decrypt the input ciphertext in-place using the AES-IGE mode, and return the
/// SHA-256 of `prefix` followed by the resulting plaintext.
///
/// This is the pair of operations that authenticates an incoming MTProto message:
/// its key is that hash of part of the authorization key and the plaintext. The
/// result is the same as [`ige_decrypt`] followed by hashing; with AES-NI the two
/// are computed together, for about a quarter less time than one after the other.
/// The buffer length must be a multiple of 16.
pub fn ige_decrypt_hashed(
    buffer: &mut [u8],
    key: &[u8; 32],
    iv: &[u8; 32],
    prefix: &[u8; 32],
) -> [u8; 32] {
    assert_eq!(buffer.len() % 16, 0);
    #[cfg(target_arch = "x86_64")]
    if x86::available() {
        // SAFETY: as in `ige_encrypt`.
        #[allow(unsafe_code)]
        return unsafe { x86::decrypt_hashed(buffer, key, iv, prefix) };
    }
    portable_decrypt_hashed(buffer, key, iv, prefix)
}

fn portable_decrypt_hashed(
    buffer: &mut [u8],
    key: &[u8; 32],
    iv: &[u8; 32],
    prefix: &[u8; 32],
) -> [u8; 32] {
    portable_decrypt(buffer, key, iv);
    crate::sha256!(prefix, &*buffer)
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

    /// FIPS 197, appendix C.3. With a zero IV, one IGE block is one AES block, so
    /// this checks the key schedule and both directions against the standard
    /// itself, not against another implementation.
    #[test]
    fn single_block_matches_the_fips_197_aes_256_vector() {
        let key: [u8; 32] = std::array::from_fn(|i| i as u8);
        let plaintext: [u8; 16] = std::array::from_fn(|i| (i * 0x11) as u8);
        let ciphertext = [
            0x8e, 0xa2, 0xb7, 0xca, 0x51, 0x67, 0x45, 0xbf, 0xea, 0xfc, 0x49, 0x90, 0x4b, 0x49,
            0x60, 0x89,
        ];
        type Pass = fn(&mut [u8], &[u8; 32], &[u8; 32]);
        let paths: [(&str, Pass, Pass); 2] = [
            ("selected", ige_encrypt, ige_decrypt),
            ("portable", portable_encrypt, portable_decrypt),
        ];
        for (path, encrypt, decrypt) in paths {
            let mut block = plaintext;
            encrypt(&mut block, &key, &[0; 32]);
            assert_eq!(block, ciphertext, "{path} encrypt");
            decrypt(&mut block, &key, &[0; 32]);
            assert_eq!(block, plaintext, "{path} decrypt");
        }
    }

    /// The path a processor selects and the portable one must agree on every
    /// length, including a final block and chains long enough to expose a
    /// mistake in either chaining value.
    #[test]
    fn selected_and_portable_paths_agree() {
        #[cfg(target_arch = "x86_64")]
        println!("AES-NI path selected: {}", x86::available());
        let lengths = (0..=40).map(|blocks| blocks * 16).chain([4096, 65536 + 16]);
        for (case, len) in lengths.enumerate() {
            let key: [u8; 32] = bytes(case as u64 * 7 + 11, 32).try_into().unwrap();
            let iv: [u8; 32] = bytes(case as u64 * 7 + 12, 32).try_into().unwrap();
            let plaintext = bytes(case as u64 * 7 + 13, len);

            let mut selected = plaintext.clone();
            let mut portable = plaintext.clone();
            ige_encrypt(&mut selected, &key, &iv);
            portable_encrypt(&mut portable, &key, &iv);
            assert_eq!(selected, portable, "encrypt {len}");
            assert!(
                len == 0 || selected != plaintext,
                "encrypt {len} changed nothing"
            );

            // Each path decrypts what the other produced.
            ige_decrypt(&mut portable, &key, &iv);
            portable_decrypt(&mut selected, &key, &iv);
            assert_eq!(portable, plaintext, "selected decrypt {len}");
            assert_eq!(selected, plaintext, "portable decrypt {len}");
        }
    }

    /// Decrypting and hashing together must give what one after the other gives,
    /// for every way the plaintext can end against a hash block: the prefix makes
    /// hash blocks straddle the 64-byte steps, and the padding takes one block or
    /// two depending on the last few bytes.
    #[test]
    fn decrypting_and_hashing_together_matches_one_after_the_other() {
        use sha2::{Digest, Sha256};
        #[cfg(target_arch = "x86_64")]
        println!("AES-NI path selected: {}", x86::available());
        let lengths = (0..=40)
            .map(|blocks| blocks * 16)
            .chain([4096, 4112, 65536 + 48, 1 << 20]);
        for (case, len) in lengths.enumerate() {
            let key: [u8; 32] = bytes(case as u64 * 5 + 21, 32).try_into().unwrap();
            let iv: [u8; 32] = bytes(case as u64 * 5 + 22, 32).try_into().unwrap();
            let prefix: [u8; 32] = bytes(case as u64 * 5 + 23, 32).try_into().unwrap();
            let ciphertext = bytes(case as u64 * 5 + 24, len);

            let mut separate = ciphertext.clone();
            reference_decrypt(&mut separate, &key, &iv);
            let expected: [u8; 32] = Sha256::new()
                .chain_update(prefix)
                .chain_update(&separate)
                .finalize()
                .into();

            let mut selected = ciphertext.clone();
            let digest = ige_decrypt_hashed(&mut selected, &key, &iv, &prefix);
            assert_eq!(selected, separate, "plaintext {len}");
            assert_eq!(digest, expected, "digest {len}");

            let mut portable = ciphertext.clone();
            let digest = portable_decrypt_hashed(&mut portable, &key, &iv, &prefix);
            assert_eq!(portable, separate, "portable plaintext {len}");
            assert_eq!(digest, expected, "portable digest {len}");
        }
    }

    /// The digest is of the prefix and the plaintext, in that order, and of
    /// nothing else: each bit of either changes it, at either end.
    #[test]
    fn the_digest_covers_the_prefix_and_every_plaintext_byte() {
        let (key, iv, prefix) = ([7u8; 32], [9u8; 32], [3u8; 32]);
        for len in [16usize, 64, 128, 208] {
            let plaintext = bytes(len as u64, len);
            let mut ciphertext = plaintext.clone();
            ige_encrypt(&mut ciphertext, &key, &iv);
            let digest_of = |ciphertext: &[u8], prefix: &[u8; 32]| {
                let mut buffer = ciphertext.to_vec();
                (ige_decrypt_hashed(&mut buffer, &key, &iv, prefix), buffer)
            };
            let (digest, decrypted) = digest_of(&ciphertext, &prefix);
            assert_eq!(decrypted, plaintext);
            for bit in [0, 255] {
                let mut other = prefix;
                other[bit / 8] ^= 1 << (bit % 8);
                assert_ne!(digest_of(&ciphertext, &other).0, digest, "prefix bit {bit}");
            }
            for byte in [0, len / 2, len - 1] {
                let mut other = ciphertext.clone();
                other[byte] ^= 0x80;
                assert_ne!(digest_of(&other, &prefix).0, digest, "byte {byte} of {len}");
            }
        }
    }

    /// Keys and IVs at the extremes of each byte, where a wrong shift or a wrong
    /// round constant in the key schedule shows.
    #[test]
    fn selected_and_portable_paths_agree_on_edge_keys() {
        for fill in [0x00u8, 0x01, 0x7f, 0x80, 0xfe, 0xff] {
            for len in [16, 32, 80] {
                let plaintext = bytes(fill as u64 + 1, len);
                let (mut selected, mut portable) = (plaintext.clone(), plaintext.clone());
                ige_encrypt(&mut selected, &[fill; 32], &[!fill; 32]);
                portable_encrypt(&mut portable, &[fill; 32], &[!fill; 32]);
                assert_eq!(selected, portable, "encrypt fill {fill:#x} len {len}");
                ige_decrypt(&mut selected, &[fill; 32], &[!fill; 32]);
                assert_eq!(selected, plaintext, "decrypt fill {fill:#x} len {len}");
            }
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
        type Pass = fn(&mut [u8], &[u8; 32], &[u8; 32]);
        let passes: [(&str, Pass); 6] = [
            ("encrypt per-block", reference_encrypt),
            ("encrypt portable", portable_encrypt),
            ("encrypt selected", ige_encrypt),
            ("decrypt per-block", reference_decrypt),
            ("decrypt portable", portable_decrypt),
            ("decrypt selected", ige_decrypt),
        ];
        for len in [64usize, 448, 656, 1600, 2096, 16 * 1024, 1024 * 1024] {
            let mut buffer = bytes(len as u64, len);
            let iterations = (8_000_000 / len).max(8);
            let mut samples = vec![Vec::new(); passes.len()];
            for round in 0..8 {
                for k in 0..passes.len() {
                    let k = (k + round) % passes.len();
                    let began = Instant::now();
                    for _ in 0..iterations {
                        (passes[k].1)(black_box(&mut buffer), &key, &iv);
                    }
                    samples[k].push(began.elapsed().as_nanos() as f64 / iterations as f64);
                }
            }
            let report: Vec<String> = passes
                .iter()
                .zip(&mut samples)
                .map(|((name, _), sample)| {
                    sample.sort_by(f64::total_cmp);
                    format!("{name} {:.1} ns", sample[4])
                })
                .collect();
            println!("IGE {len} B: {}", report.join(", "));
        }
    }

    #[test]
    #[ignore = "local decrypt-and-authenticate comparison; run release with nocapture"]
    fn benchmark_decrypt_hashed() {
        use std::hint::black_box;
        use std::time::Instant;
        let (key, iv, prefix) = ([3u8; 32], [5u8; 32], [8u8; 32]);
        type Pass = fn(&mut [u8], &[u8; 32], &[u8; 32], &[u8; 32]) -> [u8; 32];
        fn one_after_the_other(
            buffer: &mut [u8],
            key: &[u8; 32],
            iv: &[u8; 32],
            prefix: &[u8; 32],
        ) -> [u8; 32] {
            ige_decrypt(buffer, key, iv);
            crate::sha256!(prefix, &*buffer)
        }
        let passes: [(&str, Pass); 3] = [
            ("portable", portable_decrypt_hashed),
            ("one after the other", one_after_the_other),
            ("together", ige_decrypt_hashed),
        ];
        for len in [96usize, 448, 656, 1600, 16 * 1024, 1024 * 1024] {
            let mut buffer = bytes(len as u64, len);
            let iterations = (8_000_000 / len).max(8);
            let mut samples = vec![Vec::new(); passes.len()];
            for round in 0..8 {
                for k in 0..passes.len() {
                    let k = (k + round) % passes.len();
                    let began = Instant::now();
                    for _ in 0..iterations {
                        black_box((passes[k].1)(black_box(&mut buffer), &key, &iv, &prefix));
                    }
                    samples[k].push(began.elapsed().as_nanos() as f64 / iterations as f64);
                }
            }
            let report: Vec<String> = passes
                .iter()
                .zip(&mut samples)
                .map(|((name, _), sample)| {
                    sample.sort_by(f64::total_cmp);
                    format!("{name} {:.1} ns", sample[4])
                })
                .collect();
            println!("decrypt and hash {len} B: {}", report.join(", "));
        }
    }
}
