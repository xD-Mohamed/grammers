// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Types and traits involving the deserialization of the generated objects.

use std::fmt;

/// Errors that can be produced by [`Cursor`].
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// The end of the buffer was reached earlier than anticipated, which
    /// implies there is not enough data to complete the deserialization.
    UnexpectedEof,

    /// The error type indicating an unexpected constructor was found,
    /// for example, when reading data that doesn't represent the
    /// correct type (e.g. reading a `bool` when we expect a `Vec`).
    /// In particular, it can occur in the following situations:
    ///
    /// * When reading a boolean.
    /// * When reading a boxed vector.
    /// * When reading an arbitrary boxed type.
    ///
    /// It is important to note that unboxed or bare [`types`] lack the
    /// constructor information, and as such they cannot be validated.
    ///
    /// [`types`]: crate::types
    UnexpectedConstructor {
        /// The unexpected constructor identifier.
        id: u32,
    },
}

impl std::error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::UnexpectedEof => write!(f, "unexpected eof"),
            Self::UnexpectedConstructor { id } => write!(f, "unexpected constructor: {id:08x}"),
        }
    }
}

/// Re-implementation of [`std::io::Cursor`] to only work over in-memory buffers and greatly
/// narrow the possible error cases.
pub struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    /// Constructors a cursor from a slice of bytes.
    pub fn from_slice(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    // TODO not a fan we need to expose this (and a way to create `Cursor`),
    //      but crypto needs it because it needs to know where deserialization
    //      of some inner data ends.
    /// Returns the cursor's position into the slice buffer.
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Reads a single byte from the slice buffer.
    pub fn read_byte(&mut self) -> Result<u8> {
        if self.pos < self.buf.len() {
            let byte = self.buf[self.pos];
            self.pos += 1;
            Ok(byte)
        } else {
            Err(Error::UnexpectedEof)
        }
    }

    /// Borrows the next `len` bytes without allocating or copying them.
    /// The cursor is unchanged when there are not enough bytes.
    pub fn read_slice(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(len)
            .filter(|end| *end <= self.buf.len())
            .ok_or(Error::UnexpectedEof)?;
        let result = &self.buf[self.pos..end];
        self.pos = end;
        Ok(result)
    }

    /// Borrows a TL byte-string, consuming its length prefix and padding.
    /// This follows the same encoding as `Vec<u8>::deserialize`.
    pub fn read_bytes(&mut self) -> Result<&'a [u8]> {
        let first = self.read_byte()?;
        let (len, padding) = if first == 254 {
            let bytes = self.read_slice(3)?;
            let len = usize::from(bytes[0])
                | (usize::from(bytes[1]) << 8)
                | (usize::from(bytes[2]) << 16);
            (len, len % 4)
        } else {
            let len = usize::from(first);
            (len, (len + 1) % 4)
        };
        let result = self.read_slice(len)?;
        if padding != 0 {
            self.read_slice(4 - padding)?;
        }
        Ok(result)
    }

    /// Reads an exact amount of bytes to fill the input buffer.
    pub fn read_exact(&mut self, buf: &mut [u8]) -> Result<()> {
        if self.pos + buf.len() > self.buf.len() {
            Err(Error::UnexpectedEof)
        } else {
            buf.copy_from_slice(&self.buf[self.pos..self.pos + buf.len()]);
            self.pos += buf.len();
            Ok(())
        }
    }

    /// Reads all the way to the end of the slice buffer.
    pub fn read_to_end(&mut self, buf: &mut Vec<u8>) -> Result<usize> {
        buf.extend(&self.buf[self.pos..]);
        let old = self.pos;
        self.pos = self.buf.len();
        Ok(self.pos - old)
    }
}

/// Alias over a mutable reference to a [`Cursor`].
pub(crate) type Buffer<'a, 'b> = &'a mut Cursor<'b>;

/// A specialized `Result` type for deserialization operations.
pub type Result<T> = std::result::Result<T, Error>;

/// This trait allows for data serialized according to the
/// [Binary Data Serialization] to be deserialized into concrete instances.
///
/// [Binary Data Serialization]: https://core.telegram.org/mtproto/serialize
pub trait Deserializable {
    /// Deserializes an instance of the type from a given buffer.
    fn deserialize(buf: Buffer) -> Result<Self>
    where
        Self: std::marker::Sized;

    /// Convenience function to deserialize an instance from a slice of bytes.
    ///
    /// # Examples
    ///
    /// ```
    /// use grammers_tl_types::Deserializable;
    ///
    /// assert_eq!(bool::from_bytes(&[0x37, 0x97, 0x79, 0xbc]).unwrap(), false);
    /// ```
    fn from_bytes(buf: &[u8]) -> Result<Self>
    where
        Self: std::marker::Sized,
    {
        Self::deserialize(&mut Cursor::from_slice(buf))
    }
}

impl Deserializable for bool {
    /// Deserializes a boolean according to the following definitions:
    ///
    /// * `boolFalse#bc799737 = Bool;` deserializes into `false`.
    /// * `boolTrue#997275b5 = Bool;` deserializes into `true`.
    ///
    /// # Examples
    ///
    /// ```
    /// use grammers_tl_types::Deserializable;
    ///
    /// assert_eq!(bool::from_bytes(&[0xb5, 0x75, 0x72, 0x99]).unwrap(), true);
    /// assert_eq!(bool::from_bytes(&[0x37, 0x97, 0x79, 0xbc]).unwrap(), false);
    /// ```
    #[allow(clippy::unreadable_literal)]
    fn deserialize(buf: Buffer) -> Result<Self> {
        let id = u32::deserialize(buf)?;
        match id {
            0x997275b5u32 => Ok(true),
            0xbc799737u32 => Ok(false),
            _ => Err(Error::UnexpectedConstructor { id }),
        }
    }
}

impl Deserializable for i32 {
    /// Deserializes a 32-bit signed integer according to the following
    /// definition:
    ///
    /// * `int ? = Int;`.
    ///
    /// # Examples
    ///
    /// ```
    /// use grammers_tl_types::Deserializable;
    ///
    /// assert_eq!(i32::from_bytes(&[0x00, 0x00, 0x00, 0x00]).unwrap(), 0i32);
    /// assert_eq!(i32::from_bytes(&[0x01, 0x00, 0x00, 0x00]).unwrap(), 1i32);
    /// assert_eq!(i32::from_bytes(&[0xff, 0xff, 0xff, 0xff]).unwrap(), -1i32);
    /// assert_eq!(i32::from_bytes(&[0xff, 0xff, 0xff, 0x7f]).unwrap(), i32::max_value());
    /// assert_eq!(i32::from_bytes(&[0x00, 0x00, 0x00, 0x80]).unwrap(), i32::min_value());
    /// ```
    fn deserialize(buf: Buffer) -> Result<Self> {
        let mut buffer = [0u8; 4];
        buf.read_exact(&mut buffer)?;
        Ok(Self::from_le_bytes(buffer))
    }
}

impl Deserializable for u32 {
    /// Deserializes a 32-bit unsigned integer according to the following
    /// definition:
    ///
    /// * `int ? = Int;`.
    ///
    /// # Examples
    ///
    /// ```
    /// use grammers_tl_types::Deserializable;
    ///
    /// assert_eq!(u32::from_bytes(&[0x00, 0x00, 0x00, 0x00]).unwrap(), 0u32);
    /// assert_eq!(u32::from_bytes(&[0x01, 0x00, 0x00, 0x00]).unwrap(), 1u32);
    /// assert_eq!(u32::from_bytes(&[0xff, 0xff, 0xff, 0xff]).unwrap(), u32::max_value());
    /// assert_eq!(u32::from_bytes(&[0x00, 0x00, 0x00, 0x00]).unwrap(), u32::min_value());
    /// ```
    fn deserialize(buf: Buffer) -> Result<Self> {
        let mut buffer = [0u8; 4];
        buf.read_exact(&mut buffer)?;
        Ok(Self::from_le_bytes(buffer))
    }
}

impl Deserializable for i64 {
    /// Deserializes a 64-bit signed integer according to the following
    /// definition:
    ///
    /// * `long ? = Long;`.
    ///
    /// # Examples
    ///
    /// ```
    /// use grammers_tl_types::Deserializable;
    ///
    /// assert_eq!(i64::from_bytes(&[0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0]).unwrap(), 0i64);
    /// assert_eq!(i64::from_bytes(&[0x1, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0]).unwrap(), 1i64);
    /// assert_eq!(i64::from_bytes(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]).unwrap(), (-1i64));
    /// assert_eq!(i64::from_bytes(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f]).unwrap(), i64::max_value());
    /// assert_eq!(i64::from_bytes(&[0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x80]).unwrap(), i64::min_value());
    /// ```
    fn deserialize(buf: Buffer) -> Result<Self> {
        let mut buffer = [0u8; 8];
        buf.read_exact(&mut buffer)?;
        Ok(Self::from_le_bytes(buffer))
    }
}

impl Deserializable for [u8; 16] {
    /// Deserializes the 128-bit integer according to the following
    /// definition:
    ///
    /// * `int128 4*[ int ] = Int128;`.
    ///
    /// # Examples
    ///
    /// ```
    /// use grammers_tl_types::Deserializable;
    ///
    /// let data = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
    ///
    /// assert_eq!(<[u8; 16]>::from_bytes(&data).unwrap(), data);
    /// ```
    fn deserialize(buf: Buffer) -> Result<Self> {
        let mut buffer = [0u8; 16];
        buf.read_exact(&mut buffer)?;
        Ok(buffer)
    }
}

impl Deserializable for [u8; 32] {
    /// Deserializes the 128-bit integer according to the following
    /// definition:
    ///
    /// * `int256 8*[ int ] = Int256;`.
    ///
    /// # Examples
    ///
    /// ```
    /// use grammers_tl_types::Deserializable;
    ///
    /// let data = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17,
    ///             18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32];
    ///
    /// assert_eq!(<[u8; 32]>::from_bytes(&data).unwrap(), data);
    /// ```
    fn deserialize(buf: Buffer) -> Result<Self> {
        let mut buffer = [0u8; 32];
        buf.read_exact(&mut buffer)?;
        Ok(buffer)
    }
}

impl Deserializable for f64 {
    /// Deserializes a 64-bit floating point according to the
    /// following definition:
    ///
    /// * `double ? = Double;`.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::f64;
    /// use grammers_tl_types::Deserializable;
    ///
    /// assert_eq!(f64::from_bytes(&[0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0]).unwrap(), 0f64);
    /// assert_eq!(f64::from_bytes(&[0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0xf8, 0x3f]).unwrap(), 1.5f64);
    /// assert_eq!(f64::from_bytes(&[0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0xf8, 0xbf]).unwrap(), -1.5f64);
    /// assert_eq!(f64::from_bytes(&[0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0xf0, 0x7f]).unwrap(), f64::INFINITY);
    /// assert_eq!(f64::from_bytes(&[0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0xf0, 0xff]).unwrap(), f64::NEG_INFINITY);
    /// ```
    fn deserialize(buf: Buffer) -> Result<Self> {
        let mut buffer = [0u8; 8];
        buf.read_exact(&mut buffer)?;
        Ok(Self::from_le_bytes(buffer))
    }
}

#[inline]
fn deserialize_vector<T: Deserializable>(buf: Buffer) -> Result<Vec<T>> {
    let len = u32::deserialize(buf)?;
    if len == 0 {
        return Ok(Vec::new());
    }

    // The wire count is untrusted. Decode one item before reserving, and cap
    // the initial allocation while allowing valid vectors to grow normally.
    let first = T::deserialize(buf)?;
    let bounded_capacity = (4096 / std::mem::size_of::<T>().max(1)).max(1);
    // A power-of-two growth base avoids retaining extra capacity for large
    // vectors whose element size does not divide the byte budget evenly.
    let initial_capacity = 1usize << bounded_capacity.ilog2();
    let mut values = Vec::with_capacity(initial_capacity.min(len as usize));
    values.push(first);
    for _ in 1..len {
        values.push(T::deserialize(buf)?);
    }
    Ok(values)
}

impl<T: Deserializable> Deserializable for Vec<T> {
    /// Deserializes a vector of deserializable items according to the
    /// following definition:
    ///
    /// * `vector#1cb5c415 {t:Type} # [ t ] = Vector t;`.
    ///
    /// # Examples
    ///
    /// ```
    /// use grammers_tl_types::Deserializable;
    ///
    /// assert_eq!(Vec::<i32>::from_bytes(&[0x15, 0xc4, 0xb5, 0x1c, 0x0, 0x0, 0x0, 0x0]).unwrap(), Vec::new());
    /// assert_eq!(Vec::<i32>::from_bytes(&[0x15, 0xc4, 0xb5, 0x1c, 0x1, 0x0, 0x0, 0x0, 0x7f, 0x0, 0x0, 0x0]).unwrap(),
    ///            vec![0x7f_i32]);
    /// ```
    #[allow(clippy::unreadable_literal)]
    #[inline]
    fn deserialize(buf: Buffer) -> Result<Self> {
        let id = u32::deserialize(buf)?;
        if id != 0x1cb5c415u32 {
            return Err(Error::UnexpectedConstructor { id });
        }
        deserialize_vector(buf)
    }
}

impl<T: Deserializable> Deserializable for crate::RawVec<T> {
    /// Deserializes a vector of deserializable items according to the
    /// following definition:
    ///
    /// * `vector#1cb5c415 {t:Type} # [ t ] = Vector t;`.
    ///
    /// # Examples
    ///
    /// ```
    /// use grammers_tl_types::{RawVec, Deserializable};
    ///
    /// assert_eq!(RawVec::<i32>::from_bytes(&[0x0, 0x0, 0x0, 0x0]).unwrap().0, Vec::<i32>::new());
    /// assert_eq!(RawVec::<i32>::from_bytes(&[0x1, 0x0, 0x0, 0x0, 0x7f, 0x0, 0x0, 0x0]).unwrap().0, vec![0x7f_i32]);
    /// ```
    #[inline]
    fn deserialize(buf: Buffer) -> Result<Self> {
        deserialize_vector(buf).map(Self)
    }
}

impl Deserializable for String {
    /// Deserializes a UTF-8 string according to the following definition:
    ///
    /// * `string ? = String;`.
    ///
    /// # Examples
    ///
    /// ```
    /// use grammers_tl_types::Deserializable;
    ///
    /// fn test_string(string: &str, prefix: &[u8], suffix: &[u8]) {
    ///    let bytes = {
    ///        let mut tmp = prefix.to_vec();
    ///        tmp.extend(string.as_bytes());
    ///        tmp.extend(suffix);
    ///        tmp
    ///    };
    ///    let expected = string.to_owned();
    ///
    ///    assert_eq!(String::from_bytes(&bytes).unwrap(), expected);
    /// }
    ///
    /// test_string("", &[0x00], &[0x00, 0x00, 0x00]);
    /// test_string("Hi", &[0x02], &[0x0]);
    /// test_string("Hi!", &[0x03], &[]);
    /// test_string("Hello", &[0x05], &[0x0, 0x0]);
    /// test_string("Hello, world!", &[0xd], &[0x0, 0x0]);
    /// test_string(
    ///     "This is a very long string, and it has to be longer than 253 \
    ///      characters, which are quite a few but we can make it! Although, \
    ///      it is quite challenging. The quick brown fox jumps over the lazy \
    ///      fox. There is still some more text we need to type. Oh, this \
    ///      sentence made it past!",
    ///      &[0xfe, 0x11, 0x01, 0x00],
    ///      &[0x00, 0x00, 0x00]
    /// );
    /// ```
    fn deserialize(buf: Buffer) -> Result<Self> {
        Ok(String::from_utf8_lossy(buf.read_bytes()?).into_owned())
    }
}

impl Deserializable for Vec<u8> {
    /// Deserializes a vector of bytes as a byte-string according to the
    /// following definition:
    ///
    /// * `string ? = String;`.
    ///
    /// # Examples
    ///
    /// ```
    /// use grammers_tl_types::{Deserializable};
    ///
    /// assert_eq!(Vec::<u8>::from_bytes(&[0x00, 0x00, 0x00, 0x00]).unwrap(), Vec::new());
    /// assert_eq!(Vec::<u8>::from_bytes(&[0x01, 0x7f, 0x00, 0x00]).unwrap(), vec![0x7f_u8]);
    /// ```
    fn deserialize(buf: Buffer) -> Result<Self> {
        Ok(buf.read_bytes()?.to_vec())
    }
}

#[cfg(test)]
mod vector_tests {
    use super::*;
    use crate::{RawVec, Serializable};
    use std::cell::Cell;

    fn boxed_bytes(count: u32, body: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0x15, 0xc4, 0xb5, 0x1c];
        bytes.extend(count.to_le_bytes());
        bytes.extend(body);
        bytes
    }

    fn both_vectors<T: Deserializable>(bytes: &[u8]) -> [Result<Vec<T>>; 2] {
        [
            Vec::<T>::from_bytes(bytes),
            RawVec::<T>::from_bytes(&bytes[4..]).map(|value| value.0),
        ]
    }

    #[test]
    fn vectors_preserve_empty_single_and_small_values() {
        for values in [vec![], vec![7], vec![-1, 0, 1, 2, 3, 4, 5, 6]] {
            for result in both_vectors::<i32>(&values.to_bytes()) {
                assert_eq!(result.unwrap(), values);
            }
        }
    }

    #[test]
    fn vectors_reject_truncation_at_each_element_and_header() {
        let bytes = vec![11_i32, 22, 33].to_bytes();
        for cut in 0..bytes.len() {
            assert_eq!(
                Vec::<i32>::from_bytes(&bytes[..cut]),
                Err(Error::UnexpectedEof),
                "boxed vector truncated at {cut}"
            );
        }
        for cut in 0..bytes.len() - 4 {
            assert_eq!(
                RawVec::<i32>::from_bytes(&bytes[4..4 + cut]),
                Err(Error::UnexpectedEof),
                "bare vector truncated at {cut}"
            );
        }
    }

    #[test]
    fn vectors_reject_unbounded_counts_with_short_input() {
        for body in [&[][..], &[1, 0, 0, 0][..], &[1, 0, 0, 0, 2][..]] {
            for result in both_vectors::<i32>(&boxed_bytes(u32::MAX, body)) {
                assert_eq!(result, Err(Error::UnexpectedEof));
            }
        }
    }

    #[test]
    fn vectors_preserve_constructor_validation() {
        assert_eq!(
            Vec::<i32>::from_bytes(&[0, 0, 0, 0]),
            Err(Error::UnexpectedConstructor { id: 0 })
        );
        let bytes = boxed_bytes(2, &[0xb5, 0x75, 0x72, 0x99, 1, 0, 0, 0]);
        for result in both_vectors::<bool>(&bytes) {
            assert_eq!(result, Err(Error::UnexpectedConstructor { id: 1 }));
        }
    }

    #[derive(Debug, PartialEq)]
    struct ZeroSized;

    impl Deserializable for ZeroSized {
        fn deserialize(_: Buffer) -> Result<Self> {
            Ok(Self)
        }
    }

    #[derive(Debug, PartialEq)]
    struct ZeroBytes(u32);

    impl Deserializable for ZeroBytes {
        fn deserialize(_: Buffer) -> Result<Self> {
            Ok(Self(7))
        }
    }

    #[test]
    fn vectors_accept_zero_sized_and_zero_byte_elements() {
        for count in [0, 1, 8, 32] {
            let bytes = boxed_bytes(count, &[]);
            for result in both_vectors::<ZeroSized>(&bytes) {
                assert_eq!(result.unwrap().len(), count as usize);
            }
            for result in both_vectors::<ZeroBytes>(&bytes) {
                assert_eq!(
                    result.unwrap(),
                    (0..count).map(|_| ZeroBytes(7)).collect::<Vec<_>>()
                );
            }
        }
    }

    #[derive(Debug, PartialEq)]
    struct LargeElement([u8; 8192]);

    impl Deserializable for LargeElement {
        fn deserialize(buf: Buffer) -> Result<Self> {
            Ok(Self([buf.read_byte()?; 8192]))
        }
    }

    #[test]
    fn vectors_accept_elements_larger_than_initial_reservation_cap() {
        for result in both_vectors::<LargeElement>(&boxed_bytes(2, &[3, 5])) {
            assert_eq!(
                result.unwrap(),
                vec![LargeElement([3; 8192]), LargeElement([5; 8192])]
            );
        }
    }

    thread_local! {
        static DROPPED: Cell<usize> = const { Cell::new(0) };
    }

    #[derive(Debug)]
    struct DropElement;

    impl Deserializable for DropElement {
        fn deserialize(buf: Buffer) -> Result<Self> {
            bool::deserialize(buf).map(|_| Self)
        }
    }

    impl Drop for DropElement {
        fn drop(&mut self) {
            DROPPED.set(DROPPED.get() + 1);
        }
    }

    #[test]
    fn vectors_drop_completed_elements_on_first_middle_and_last_errors() {
        for valid in 0..3 {
            for boxed in [false, true] {
                DROPPED.set(0);
                let mut body = Vec::new();
                for _ in 0..valid {
                    body.extend([0xb5, 0x75, 0x72, 0x99]);
                }
                body.extend([1, 0, 0, 0]);
                body.extend([0xb5, 0x75, 0x72, 0x99]);
                let bytes = boxed_bytes(3, &body);
                let mut cursor = Cursor::from_slice(if boxed { &bytes } else { &bytes[4..] });
                let result = if boxed {
                    Vec::<DropElement>::deserialize(&mut cursor)
                } else {
                    RawVec::<DropElement>::deserialize(&mut cursor).map(|value| value.0)
                };
                assert!(matches!(
                    result,
                    Err(Error::UnexpectedConstructor { id: 1 })
                ));
                assert_eq!(DROPPED.get(), valid);
                assert_eq!(cursor.pos(), (if boxed { 8 } else { 4 }) + (valid + 1) * 4);
            }
        }
    }

    fn collect_vector<T: Deserializable>(bytes: &[u8]) -> Vec<T> {
        // Preserve the old decoder's Result boundary and constructor validation.
        fn deserialize<T: Deserializable>(buf: Buffer) -> Result<Vec<T>> {
            let id = u32::deserialize(buf)?;
            if id != 0x1cb5_c415 {
                return Err(Error::UnexpectedConstructor { id });
            }
            let len = u32::deserialize(buf)?;
            (0..len).map(|_| T::deserialize(buf)).collect()
        }
        deserialize(&mut Cursor::from_slice(bytes)).unwrap()
    }

    fn reserved_vector<T: Deserializable>(bytes: &[u8]) -> Vec<T> {
        Vec::<T>::from_bytes(bytes).unwrap()
    }

    fn benchmark_vector<T: Deserializable + Serializable>(name: &str, values: &[T]) {
        use std::{hint::black_box, time::Instant};

        fn elapsed<T>(bytes: &[u8], iterations: usize, decode: fn(&[u8]) -> Vec<T>) -> f64 {
            let start = Instant::now();
            for _ in 0..iterations {
                black_box(decode(black_box(bytes)));
            }
            start.elapsed().as_secs_f64() * 1e9 / iterations as f64
        }

        let mut bytes = boxed_bytes(values.len() as u32, &[]);
        for value in values {
            value.serialize(&mut bytes);
        }
        let iterations = (250_000 / values.len().max(1)).clamp(2_000, 250_000);
        let mut collect_times = [0.0_f64; 8];
        let mut reserved_times = [0.0_f64; 8];
        for _ in 0..100 {
            black_box(collect_vector::<T>(&bytes));
            black_box(reserved_vector::<T>(&bytes));
        }
        for sample in 0..8 {
            if sample % 2 == 0 {
                collect_times[sample] = elapsed(&bytes, iterations, collect_vector::<T>);
                reserved_times[sample] = elapsed(&bytes, iterations, reserved_vector::<T>);
            } else {
                reserved_times[sample] = elapsed(&bytes, iterations, reserved_vector::<T>);
                collect_times[sample] = elapsed(&bytes, iterations, collect_vector::<T>);
            }
        }
        collect_times.sort_by(f64::total_cmp);
        reserved_times.sort_by(f64::total_cmp);
        let collect_ns = (collect_times[3] + collect_times[4]) / 2.0;
        let reserved_ns = (reserved_times[3] + reserved_times[4]) / 2.0;
        println!(
            "{name:>8} count={:>4}: collect={collect_ns:>9.1} ns reserved={reserved_ns:>9.1} ns ratio={:.3} capacity={}/{}",
            values.len(),
            reserved_ns / collect_ns,
            collect_vector::<T>(&bytes).capacity(),
            reserved_vector::<T>(&bytes).capacity()
        );
    }

    #[test]
    #[ignore = "local release microbenchmark; no network or live performance claim"]
    fn benchmark_vector_deserialization() {
        if cfg!(debug_assertions) {
            panic!("run this benchmark with --release");
        }
        for count in [0, 1, 8, 32, 128, 1024] {
            benchmark_vector("i32", &(0..count).collect::<Vec<i32>>());
            benchmark_vector("i64", &(0..i64::from(count)).collect::<Vec<i64>>());
            benchmark_vector(
                "string",
                &(0..count)
                    .map(|index| format!("fixture-{index:04}"))
                    .collect::<Vec<_>>(),
            );
            #[cfg(feature = "tl-api")]
            benchmark_vector(
                "Peer",
                &(0..count)
                    .map(|index| {
                        crate::enums::Peer::User(crate::types::PeerUser {
                            user_id: i64::from(index),
                        })
                    })
                    .collect::<Vec<_>>(),
            );
        }
    }
}

#[cfg(test)]
mod borrowed_bytes_tests {
    use super::*;
    use crate::Serializable;

    #[test]
    fn borrowed_strings_preserve_short_long_payloads_padding_and_positions() {
        for len in [0usize, 1, 2, 3, 4, 253, 254, 255, 256, 1024, 4097] {
            let data: Vec<_> = (0..len).map(|i| i as u8).collect();
            let bytes = data.to_bytes();
            let mut cursor = Cursor::from_slice(&bytes);
            let borrowed = cursor.read_bytes().unwrap();
            assert_eq!(borrowed, data);
            assert_eq!(Vec::<u8>::from_bytes(&bytes).unwrap(), data);
            assert_eq!(cursor.pos(), bytes.len());
            let offset = if len < 254 { 1 } else { 4 };
            assert_eq!(borrowed.as_ptr(), bytes[offset..].as_ptr());
            for cut in 0..bytes.len() {
                assert!(Cursor::from_slice(&bytes[..cut]).read_bytes().is_err());
            }
        }
    }

    #[test]
    fn borrowed_reads_reject_overflow_and_keep_lossy_string_behavior() {
        let mut cursor = Cursor::from_slice(&[1, 2, 3]);
        assert_eq!(cursor.read_slice(1).unwrap(), &[1]);
        assert_eq!(cursor.read_slice(usize::MAX), Err(Error::UnexpectedEof));
        assert_eq!(cursor.pos(), 1);
        assert_eq!(
            String::from_bytes(&vec![0xff, b'a'].to_bytes()).unwrap(),
            "\u{fffd}a"
        );
    }
}
