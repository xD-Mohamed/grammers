// Copyright 2026 - developers of the `grammers` project.
// SPDX-License-Identifier: MIT OR Apache-2.0
// Standalone, offline release probe. See README.md for scope and reproduction.
use std::hint::black_box;
use std::time::Instant;

pub struct RawVec<T>(pub Vec<T>);
#[path = "../../grammers-tl-types/src/serialize.rs"]
mod serialize;
use serialize::Serializable;

mod mtp {
    pub enum DeserializeError {
        MessageBufferTooSmall,
    }
}
#[allow(dead_code)]
#[path = "../../grammers-mtproto/src/utils.rs"]
mod utils;

// Slice append from upstream 3122892, kept separate from the public trait.
struct SliceBuffer<const N: usize> {
    array: [u8; N],
    pos: usize,
}
impl<const N: usize> SliceBuffer<N> {
    fn append(&mut self, bytes: &[u8]) {
        let end = self.pos + bytes.len();
        self.array[self.pos..end].copy_from_slice(bytes);
        self.pos = end;
    }
}

#[inline(never)]
fn header_legacy(value: i64) -> [u8; 24] {
    let mut header = utils::StackBuffer::<24>::new();
    value.serialize(&mut header);
    (value as i32).serialize(&mut header);
    1024i32.serialize(&mut header);
    0x73f1f8dcu32.serialize(&mut header);
    2i32.serialize(&mut header);
    header.into_inner()
}

#[inline(never)]
fn header_slice(value: i64) -> [u8; 24] {
    let mut header = SliceBuffer {
        array: [0; 24],
        pos: 0,
    };
    header.append(&value.to_le_bytes());
    header.append(&(value as i32).to_le_bytes());
    header.append(&1024i32.to_le_bytes());
    header.append(&0x73f1f8dcu32.to_le_bytes());
    header.append(&2i32.to_le_bytes());
    header.array
}

#[inline(never)]
fn bytes_legacy(payload: &[u8], out: &mut Vec<u8>) {
    payload.serialize(out);
}

#[inline(never)]
fn bytes_slice(payload: &[u8], out: &mut Vec<u8>) {
    // Exact byte-string implementation from upstream 3122892, with Vec sink.
    let sink = &mut |bytes: &[u8]| out.extend_from_slice(bytes);
    let len = if payload.len() <= 253 {
        sink(&[payload.len() as u8]);
        payload.len() + 1
    } else {
        sink(&[
            254,
            payload.len() as u8,
            (payload.len() >> 8) as u8,
            (payload.len() >> 16) as u8,
        ]);
        payload.len()
    };
    sink(payload);
    sink(&[0; 3][..(4 - len % 4) % 4]);
}

fn headers(f: fn(i64) -> [u8; 24], count: u32) -> f64 {
    let start = Instant::now();
    for i in 0..count {
        black_box(f(black_box(i as i64)));
    }
    start.elapsed().as_nanos() as f64 / f64::from(count)
}

fn bytes(f: fn(&[u8], &mut Vec<u8>), payload: &[u8], count: u32) -> f64 {
    let mut out = Vec::with_capacity(payload.len() + 8);
    let start = Instant::now();
    for _ in 0..count {
        out.clear();
        f(black_box(payload), black_box(&mut out));
        black_box(&out);
    }
    start.elapsed().as_nanos() as f64 / f64::from(count)
}

fn paired(name: &str, mut legacy: impl FnMut() -> f64, mut slice: impl FnMut() -> f64) {
    legacy();
    slice();
    for sample in 0..9 {
        let (a, b) = if sample % 2 == 0 {
            (legacy(), slice())
        } else {
            let b = slice();
            (legacy(), b)
        };
        println!("{name},{sample},{a:.3},{b:.3}");
    }
}

fn main() {
    for value in [0, -1, i64::MIN, i64::MAX, 123456789] {
        assert_eq!(header_legacy(value), header_slice(value));
    }
    for n in [0, 1, 2, 3, 4, 252, 253, 254, 255, 256, 4096, 65536] {
        let payload = vec![0xa5; n];
        let (mut a, mut b) = (vec![], vec![]);
        bytes_legacy(&payload, &mut a);
        bytes_slice(&payload, &mut b);
        assert_eq!(payload.to_bytes(), a);
        assert_eq!(a, b, "payload length {n}");
    }
    println!("scenario,sample,legacy_ns,slice_ns");
    paired(
        "container_header",
        || headers(header_legacy, 5_000_000),
        || headers(header_slice, 5_000_000),
    );
    for n in [16, 256, 4096, 65536] {
        let payload = vec![0xa5; n];
        let count = if n < 4096 { 2_000_000 } else { 100_000 };
        paired(
            &format!("bytes_{n}"),
            || bytes(bytes_legacy, &payload, count),
            || bytes(bytes_slice, &payload, count),
        );
    }
}
