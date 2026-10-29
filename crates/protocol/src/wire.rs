//! How values are written on the socket: integers as LEB128 varints (signed ones
//! zigzagged first), strings and lists as their length then their items, options and
//! enums as a tag byte then their fields.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    #[error("the message ends in the middle of a value")]
    Truncated,
    #[error("a varint longer than its type")]
    Overlong,
    #[error("tag {tag} is not a {kind}")]
    UnknownTag { kind: &'static str, tag: u8 },
    #[error("text that is not UTF-8")]
    NotUtf8,
    #[error("{0} is not a character")]
    NotChar(u32),
    #[error("{0} bytes left over after the message")]
    LeftOver(usize),
}

pub trait Wire: Sized {
    fn put(&self, out: &mut Vec<u8>);

    /// Reads a value from the front of `input`, leaving the rest.
    ///
    /// # Errors
    ///
    /// When the bytes are not a value of the type.
    fn take(input: &mut &[u8]) -> Result<Self, DecodeError>;
}

/// Writes `value` alone, as a message's payload.
pub fn encode<T: Wire>(value: &T) -> Vec<u8> {
    let mut out = Vec::new();
    value.put(&mut out);
    out
}

/// Reads a whole payload as one `T`.
///
/// # Errors
///
/// When the bytes are not a `T`, or are more than one.
pub fn decode<T: Wire>(mut input: &[u8]) -> Result<T, DecodeError> {
    let value = T::take(&mut input)?;
    if input.is_empty() {
        Ok(value)
    } else {
        Err(DecodeError::LeftOver(input.len()))
    }
}

pub(crate) fn byte(input: &mut &[u8]) -> Result<u8, DecodeError> {
    let (&first, rest) = input.split_first().ok_or(DecodeError::Truncated)?;
    *input = rest;
    Ok(first)
}

fn put_varint(mut value: u64, out: &mut Vec<u8>) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn take_varint(input: &mut &[u8], bits: u32) -> Result<u64, DecodeError> {
    let mut value = 0u128;
    let mut shift = 0;
    loop {
        let next = byte(input)?;
        if shift >= bits {
            return Err(DecodeError::Overlong);
        }
        value |= u128::from(next & 0x7f) << shift;
        if next & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    if value >> bits != 0 {
        return Err(DecodeError::Overlong);
    }
    Ok(value as u64)
}

macro_rules! unsigned {
    ($($ty:ty),*) => {$(
        impl Wire for $ty {
            fn put(&self, out: &mut Vec<u8>) {
                put_varint(u64::from(*self), out);
            }

            fn take(input: &mut &[u8]) -> Result<Self, DecodeError> {
                let value = take_varint(input, <$ty>::BITS)?;
                <$ty>::try_from(value).map_err(|_| DecodeError::Overlong)
            }
        }
    )*};
}
unsigned!(u16, u32, u64);

impl Wire for u8 {
    fn put(&self, out: &mut Vec<u8>) {
        out.push(*self);
    }

    fn take(input: &mut &[u8]) -> Result<Self, DecodeError> {
        byte(input)
    }
}

impl Wire for i32 {
    fn put(&self, out: &mut Vec<u8>) {
        let zigzag = (self << 1) ^ (self >> 31);
        zigzag.cast_unsigned().put(out);
    }

    fn take(input: &mut &[u8]) -> Result<Self, DecodeError> {
        let zigzag = u32::take(input)?;
        Ok((zigzag >> 1).cast_signed() ^ -(zigzag & 1).cast_signed())
    }
}

impl Wire for bool {
    fn put(&self, out: &mut Vec<u8>) {
        out.push(u8::from(*self));
    }

    fn take(input: &mut &[u8]) -> Result<Self, DecodeError> {
        match byte(input)? {
            0 => Ok(false),
            1 => Ok(true),
            tag => Err(DecodeError::UnknownTag { kind: "bool", tag }),
        }
    }
}

impl Wire for char {
    fn put(&self, out: &mut Vec<u8>) {
        u32::from(*self).put(out);
    }

    fn take(input: &mut &[u8]) -> Result<Self, DecodeError> {
        let value = u32::take(input)?;
        char::from_u32(value).ok_or(DecodeError::NotChar(value))
    }
}

fn take_bytes<'a>(input: &mut &'a [u8]) -> Result<&'a [u8], DecodeError> {
    let length = usize::try_from(u64::take(input)?).map_err(|_| DecodeError::Truncated)?;
    if length > input.len() {
        return Err(DecodeError::Truncated);
    }
    let (bytes, rest) = input.split_at(length);
    *input = rest;
    Ok(bytes)
}

impl Wire for String {
    fn put(&self, out: &mut Vec<u8>) {
        (self.len() as u64).put(out);
        out.extend_from_slice(self.as_bytes());
    }

    fn take(input: &mut &[u8]) -> Result<Self, DecodeError> {
        let bytes = take_bytes(input)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| DecodeError::NotUtf8)
    }
}

impl Wire for Box<str> {
    fn put(&self, out: &mut Vec<u8>) {
        (self.len() as u64).put(out);
        out.extend_from_slice(self.as_bytes());
    }

    fn take(input: &mut &[u8]) -> Result<Self, DecodeError> {
        String::take(input).map(String::into_boxed_str)
    }
}

/// Bytes that need not be text: what is typed, paths.
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct Bytes(pub Vec<u8>);

impl fmt::Debug for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", String::from_utf8_lossy(&self.0))
    }
}

impl Wire for Bytes {
    fn put(&self, out: &mut Vec<u8>) {
        (self.0.len() as u64).put(out);
        out.extend_from_slice(&self.0);
    }

    fn take(input: &mut &[u8]) -> Result<Self, DecodeError> {
        take_bytes(input).map(|bytes| Self(bytes.to_vec()))
    }
}

impl<T: Wire> Wire for Vec<T> {
    fn put(&self, out: &mut Vec<u8>) {
        (self.len() as u64).put(out);
        for item in self {
            item.put(out);
        }
    }

    fn take(input: &mut &[u8]) -> Result<Self, DecodeError> {
        let length = u64::take(input)?;
        // Every item takes a byte at least, so a length past what is left is a lie.
        if length > input.len() as u64 {
            return Err(DecodeError::Truncated);
        }
        (0..length).map(|_| T::take(input)).collect()
    }
}

impl<T: Wire> Wire for Option<T> {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            None => out.push(0),
            Some(value) => {
                out.push(1);
                value.put(out);
            }
        }
    }

    fn take(input: &mut &[u8]) -> Result<Self, DecodeError> {
        match byte(input)? {
            0 => Ok(None),
            1 => T::take(input).map(Some),
            tag => Err(DecodeError::UnknownTag {
                kind: "option",
                tag,
            }),
        }
    }
}

impl<A: Wire, B: Wire> Wire for (A, B) {
    fn put(&self, out: &mut Vec<u8>) {
        self.0.put(out);
        self.1.put(out);
    }

    fn take(input: &mut &[u8]) -> Result<Self, DecodeError> {
        Ok((A::take(input)?, B::take(input)?))
    }
}

/// A struct written as its fields in order.
macro_rules! record {
    ($name:ident { $($field:ident),* $(,)? }) => {
        impl $crate::wire::Wire for $name {
            fn put(&self, out: &mut Vec<u8>) {
                $($crate::wire::Wire::put(&self.$field, out);)*
            }

            fn take(input: &mut &[u8]) -> Result<Self, $crate::wire::DecodeError> {
                Ok(Self { $($field: $crate::wire::Wire::take(input)?),* })
            }
        }
    };
}

/// An enum written as a tag byte, then the fields of its variant in order. Tags are
/// never reused: a variant that goes keeps its tag unused.
macro_rules! variants {
    ($name:ident { $($tag:literal => $variant:ident $({ $($field:ident),* $(,)? })?),* $(,)? }) => {
        impl $crate::wire::Wire for $name {
            fn put(&self, out: &mut Vec<u8>) {
                match self {
                    $(Self::$variant $({ $($field),* })? => {
                        out.push($tag);
                        $($($crate::wire::Wire::put($field, out);)*)?
                    })*
                }
            }

            fn take(input: &mut &[u8]) -> Result<Self, $crate::wire::DecodeError> {
                match $crate::wire::byte(input)? {
                    $($tag => Ok(Self::$variant $({ $($field: $crate::wire::Wire::take(input)?),* })?),)*
                    tag => Err($crate::wire::DecodeError::UnknownTag { kind: stringify!($name), tag }),
                }
            }
        }
    };
}

pub(crate) use {record, variants};

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T: Wire + PartialEq + fmt::Debug>(value: T) {
        assert_eq!(decode::<T>(&encode(&value)), Ok(value));
    }

    #[test]
    fn integers_text_and_lists_read_back_as_written() {
        for value in [0u32, 1, 127, 128, 300, u32::MAX] {
            round_trip(value);
        }
        for value in [0i32, -1, 1, -64, 64, i32::MIN, i32::MAX] {
            round_trip(value);
        }
        round_trip(u64::MAX);
        round_trip("héllo 日本".to_owned());
        round_trip(vec![Some('x'), None, Some('😀')]);
        round_trip((true, Bytes(vec![0, 255, 10])));
    }

    #[test]
    fn small_numbers_take_one_byte() {
        assert_eq!(encode(&127u32), [127]);
        assert_eq!(encode(&-1i32), [1]);
        assert_eq!(encode(&300u32), [0xac, 0x02]);
    }

    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        assert_eq!(
            decode::<u16>(&[0xff, 0xff, 0x7f]),
            Err(DecodeError::Overlong)
        );
        assert_eq!(decode::<u32>(&[0x80]), Err(DecodeError::Truncated));
        assert_eq!(decode::<String>(&[5, b'a']), Err(DecodeError::Truncated));
        assert_eq!(decode::<String>(&[1, 0xff]), Err(DecodeError::NotUtf8));
        assert_eq!(
            decode::<Vec<u8>>(&[0xff, 0xff, 0xff, 0x0f]),
            Err(DecodeError::Truncated)
        );
        assert_eq!(
            decode::<char>(&[0x80, 0xb0, 0x03]),
            Err(DecodeError::NotChar(0xd800))
        );
        assert_eq!(decode::<u8>(&[1, 2]), Err(DecodeError::LeftOver(1)));
        assert!(matches!(
            decode::<bool>(&[2]),
            Err(DecodeError::UnknownTag { .. })
        ));
    }
}
