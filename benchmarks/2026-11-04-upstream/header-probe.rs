// Copyright 2026 - developers of the `grammers` project.
// SPDX-License-Identifier: MIT OR Apache-2.0
// Offline paired probe of the compatible StackBuffer change from upstream 3122892.
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

// Exact pre-change cursor implementation from b48890d, grammers-mtproto/src/utils.rs.
struct Baseline<const N: usize> {
    array: [u8; N],
    pos: usize,
}
impl<const N: usize> Extend<u8> for Baseline<N> {
    fn extend<T: IntoIterator<Item = u8>>(&mut self, iter: T) {
        iter.into_iter().for_each(|x| {
            self.array[self.pos] = x;
            self.pos += 1;
        });
    }
}

type HeaderFn<const N: usize> = fn(i64, i64, i32, i32, i32) -> [u8; N];

#[inline(never)]
fn baseline_packet(value: i64, salt: i64, _seq: i32, _len: i32, _count: i32) -> [u8; 16] {
    let mut header = Baseline {
        array: [0; 16],
        pos: 0,
    };
    value.serialize(&mut header);
    salt.serialize(&mut header);
    header.array
}

#[inline(never)]
fn current_packet(value: i64, salt: i64, _seq: i32, _len: i32, _count: i32) -> [u8; 16] {
    let mut header = utils::StackBuffer::<16>::new();
    value.serialize(&mut header);
    salt.serialize(&mut header);
    header.into_inner()
}

#[inline(never)]
fn baseline_container(value: i64, _salt: i64, seq: i32, len: i32, count: i32) -> [u8; 24] {
    let mut header = Baseline {
        array: [0; 24],
        pos: 0,
    };
    value.serialize(&mut header);
    seq.serialize(&mut header);
    len.serialize(&mut header);
    0x73f1f8dcu32.serialize(&mut header);
    count.serialize(&mut header);
    header.array
}

#[inline(never)]
fn current_container(value: i64, _salt: i64, seq: i32, len: i32, count: i32) -> [u8; 24] {
    let mut header = utils::StackBuffer::<24>::new();
    value.serialize(&mut header);
    seq.serialize(&mut header);
    len.serialize(&mut header);
    0x73f1f8dcu32.serialize(&mut header);
    count.serialize(&mut header);
    header.into_inner()
}

fn measure<const N: usize>(f: HeaderFn<N>) -> f64 {
    const COUNT: u32 = 10_000_000;
    let start = Instant::now();
    for i in 0..COUNT {
        let value = i64::from(i).wrapping_mul(0x123456789abcdef);
        black_box(f(
            black_box(value),
            black_box(!value),
            black_box(i as i32),
            black_box(1024),
            black_box(2),
        ));
    }
    start.elapsed().as_nanos() as f64 / f64::from(COUNT)
}

fn paired<const N: usize>(name: &str, before: HeaderFn<N>, after: HeaderFn<N>) {
    for value in [0, -1, i64::MIN, i64::MAX, 123456789] {
        assert_eq!(
            before(value, !value, value as i32, 1024, 2),
            after(value, !value, value as i32, 1024, 2)
        );
    }
    measure(before);
    measure(after);
    for sample in 0..15 {
        let (a, b) = if sample % 2 == 0 {
            (measure(before), measure(after))
        } else {
            let b = measure(after);
            (measure(before), b)
        };
        println!("{name},{sample},{a:.3},{b:.3}");
    }
}

fn main() {
    assert_eq!(0i64.to_bytes(), [0; 8]);
    println!("scenario,sample,baseline_ns,current_ns");
    paired("packet_header", baseline_packet, current_packet);
    paired("container_header", baseline_container, current_container);
}
