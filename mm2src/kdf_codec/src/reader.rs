//! Read side of the codec: [`Reader`], [`Deserializable`], the `Error`
//! type, the `CoinVariant` dispatch hint, and the [`deserialize`] helpers.
//!
//! KDF-original.

use crate::varint::CompactInteger;
use derive_more::Display;
use std::{io, marker};

/// Decode `T` from `buffer`, requiring that every byte is consumed.
pub fn deserialize<R, T>(buffer: R) -> Result<T, Error>
where
    R: io::Read,
    T: Deserializable,
{
    let mut reader = Reader::from_read(buffer);
    let value = reader.read()?;

    if reader.is_finished() {
        Ok(value)
    } else {
        Err(Error::UnreadData)
    }
}

/// Iteratively decode a stream of `T` values, exhausting the reader.
pub fn deserialize_iterator<R, T>(buffer: R) -> ReadIterator<R, T>
where
    R: io::Read,
    T: Deserializable,
{
    ReadIterator {
        reader: Reader::from_read(buffer),
        iter_type: marker::PhantomData,
    }
}

/// Most slots `read_list`/`read_list_max` reserve before any item has been decoded. Larger lists
/// (a block's transactions can run to the tens of thousands) pay a handful of doubling
/// reallocations, negligible next to decoding them.
const MAX_PREALLOCATED_ITEMS: usize = 4096;

/// Codec-level decode error.
#[derive(Debug, Display, PartialEq)]
pub enum Error {
    MalformedData,
    UnexpectedEnd,
    UnreadData,
    Custom(String),
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(_: io::Error) -> Self { Error::UnexpectedEnd }
}

/// Trait implemented by every type that can be parsed from the wire
/// using a [`Reader`].
pub trait Deserializable {
    fn deserialize<T>(reader: &mut Reader<T>) -> Result<Self, Error>
    where
        Self: Sized,
        T: io::Read;
}

/// Coin-family dispatch hint carried by the [`Reader`]. Lets the
/// `Transaction` and `BlockHeader` deserializers reach for Qtum-specific
/// fields when the input came from a Qtum chain. New variants are added
/// as Phase B reaches additional coins.
#[derive(Debug)]
pub enum CoinVariant {
    Standard,
    Qtum,
}

impl CoinVariant {
    pub fn is_qtum(&self) -> bool { matches!(self, CoinVariant::Qtum) }
}

/// Stateful reader over an arbitrary `io::Read`. Keeps a one-byte
/// look-ahead used by `is_finished`, plus the `CoinVariant` dispatch
/// hint (default `Standard`).
#[derive(Debug)]
pub struct Reader<T> {
    buffer: T,
    peeked: Option<u8>,
    coin_variant: CoinVariant,
}

impl<'a> Reader<&'a [u8]> {
    /// Construct a reader over a borrowed byte slice (variant defaults
    /// to `Standard`).
    pub fn new(buffer: &'a [u8]) -> Self {
        Reader {
            buffer,
            peeked: None,
            coin_variant: CoinVariant::Standard,
        }
    }

    /// Construct a reader with an explicit coin variant.
    pub fn new_with_coin_variant(buffer: &'a [u8], coin_variant: CoinVariant) -> Self {
        Reader {
            buffer,
            peeked: None,
            coin_variant,
        }
    }
}

impl<T> io::Read for Reader<T>
where
    T: io::Read,
{
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, io::Error> {
        // Fast path: nothing peeked, delegate straight to the underlying
        // reader. Only when a one-byte look-ahead is held do we need the
        // splice logic below.
        match self.peeked.take() {
            None => io::Read::read(&mut self.buffer, buf),
            Some(p) if buf.is_empty() => {
                self.peeked = Some(p);
                Ok(0)
            },
            Some(p) => {
                buf[0] = p;
                io::Read::read(&mut self.buffer, &mut buf[1..]).map(|n| n + 1)
            },
        }
    }
}

impl<R> Reader<R>
where
    R: io::Read,
{
    /// Construct a reader over an arbitrary `io::Read`.
    pub fn from_read(read: R) -> Self {
        Reader {
            buffer: read,
            peeked: None,
            coin_variant: CoinVariant::Standard,
        }
    }

    /// Decode a single value of type `T`.
    pub fn read<T>(&mut self) -> Result<T, Error>
    where
        T: Deserializable,
    {
        T::deserialize(self)
    }

    /// Decode while observing every consumed byte through `proxy`.
    /// Used by the SPV codepaths to feed bytes to a hasher concurrently
    /// with parsing.
    pub fn read_with_proxy<T, F>(&mut self, proxy: F) -> Result<T, Error>
    where
        T: Deserializable,
        F: FnMut(&[u8]),
    {
        let mut reader = Reader::from_read(Proxy::new(self, proxy));
        T::deserialize(&mut reader)
    }

    /// Read exactly `bytes.len()` bytes into the destination.
    pub fn read_slice(&mut self, bytes: &mut [u8]) -> Result<(), Error> {
        io::Read::read_exact(self, bytes).map_err(|_| Error::UnexpectedEnd)
    }

    /// Decode a length-prefixed list of `T` (CompactInteger length, then
    /// that many `T` values).
    pub fn read_list<T>(&mut self) -> Result<Vec<T>, Error>
    where
        T: Deserializable,
    {
        let len: usize = self.read::<CompactInteger>()?.into();
        self.read_items(len)
    }

    /// Reads `len` items. The length comes from the wire, so it is never trusted for
    /// preallocation: at most `MAX_PREALLOCATED_ITEMS` slots are reserved up front and the vector
    /// grows (amortized doubling) only as items are actually decoded. A hostile length therefore
    /// ends at the first missing byte with `UnexpectedEnd` instead of asking the allocator for it.
    fn read_items<T>(&mut self, len: usize) -> Result<Vec<T>, Error>
    where
        T: Deserializable,
    {
        let mut out = Vec::with_capacity(len.min(MAX_PREALLOCATED_ITEMS));
        for _ in 0..len {
            out.push(self.read()?);
        }
        Ok(out)
    }

    /// Like [`Self::read_list`] but rejects lengths above `max` to bound
    /// allocations against malicious inputs.
    pub fn read_list_max<T>(&mut self, max: usize) -> Result<Vec<T>, Error>
    where
        T: Deserializable,
    {
        let len: usize = self.read::<CompactInteger>()?.into();
        if len > max {
            return Err(Error::MalformedData);
        }
        self.read_items(len)
    }

    /// Has every byte been consumed? Side-effects the reader by peeking
    /// one byte ahead, hence taking `&mut self`.
    #[allow(clippy::wrong_self_convention)]
    pub fn is_finished(&mut self) -> bool {
        if self.peeked.is_some() {
            return false;
        }
        let mut probe = [0u8];
        match self.read_slice(&mut probe) {
            Ok(_) => {
                self.peeked = Some(probe[0]);
                false
            },
            Err(_) => true,
        }
    }

    /// Borrow the active coin variant.
    pub fn coin_variant(&self) -> &CoinVariant { &self.coin_variant }
}

/// Iterator that lazily decodes a homogeneous stream of `T` until the
/// underlying reader is exhausted.
pub struct ReadIterator<R, T> {
    reader: Reader<R>,
    iter_type: marker::PhantomData<T>,
}

impl<R, T> Iterator for ReadIterator<R, T>
where
    R: io::Read,
    T: Deserializable,
{
    type Item = Result<T, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.reader.is_finished() {
            None
        } else {
            Some(self.reader.read())
        }
    }
}

/// `io::Read` adapter that mirrors every consumed byte to a closure.
struct Proxy<F, T> {
    from: F,
    observe: T,
}

impl<F, T> Proxy<F, T> {
    fn new(from: F, observe: T) -> Self { Proxy { from, observe } }
}

impl<F, T> io::Read for Proxy<F, T>
where
    F: io::Read,
    T: FnMut(&[u8]),
{
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, io::Error> {
        let n = io::Read::read(&mut self.from, buf)?;
        (self.observe)(&buf[..n]);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_list_length_ends_with_unexpected_end() {
        let mut bytes = vec![0xff];
        bytes.extend_from_slice(&u64::MAX.to_le_bytes());
        bytes.extend_from_slice(&[1, 2, 3]);
        let mut reader = Reader::from_read(bytes.as_slice());
        assert_eq!(reader.read_list::<u8>(), Err(Error::UnexpectedEnd));

        let mut reader = Reader::from_read(bytes.as_slice());
        assert_eq!(reader.read_list_max::<u8>(usize::MAX), Err(Error::UnexpectedEnd));
    }

    #[test]
    fn list_longer_than_the_preallocation_cap_still_decodes() {
        let n = MAX_PREALLOCATED_ITEMS * 3 + 7;
        let mut bytes = vec![0xfe];
        bytes.extend_from_slice(&(n as u32).to_le_bytes());
        bytes.extend((0..n).map(|i| i as u8));
        let mut reader = Reader::from_read(bytes.as_slice());
        let list: Vec<u8> = reader.read_list().unwrap();
        assert_eq!(list.len(), n);
        assert_eq!(list[n - 1], (n - 1) as u8);
    }
}
