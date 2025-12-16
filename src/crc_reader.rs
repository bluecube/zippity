use std::{
    io::{Result, SeekFrom},
    pin::Pin,
    task::{Context, Poll, ready},
};

use pin_project::pin_project;
use tokio::io::ReadBuf;

use crate::entry_data::EntryReader;

/// Wraps an existing `EntryReader` and calculates CRC32 while reading from it.
#[pin_project]
pub struct CrcReader<T> {
    #[pin]
    inner_reader: T,
    hasher: crc32fast::Hasher,
    did_seek: bool,
    seek_in_progress: bool,
}

impl<T> CrcReader<T> {
    pub fn new(inner_reader: T) -> Self {
        CrcReader {
            inner_reader,
            hasher: crc32fast::Hasher::new(),
            did_seek: false,
            seek_in_progress: false,
        }
    }

    pub fn get_crc32(&self) -> u32 {
        // Cloning as a workaround -- finalize consumes, but we only have the hasher borrowed
        self.hasher.clone().finalize()
    }

    pub fn is_crc_valid(&self) -> bool {
        !self.did_seek
    }
}

impl<T> CrcReader<T> {
    /// This is method adapts the two phase seek from `tokio::AsyncSeek` to a simpler
    /// single call interface similar to `futures::io::AsyncSeek`.
    pub fn seek<D>(
        mut self: Pin<&mut Self>,
        data: &D,
        ctx: &mut Context<'_>,
        position: SeekFrom,
    ) -> Poll<Result<u64>>
    where
        T: EntryReader<D>,
    {
        if self.seek_in_progress {
            self.poll_seek_complete(data, ctx)
        } else {
            self.as_mut().start_seek(data, position)?;
            self.as_mut().poll_seek_complete(data, ctx)
        }
    }

    pub fn poll_read<D>(
        self: Pin<&mut Self>,
        data: &D,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<()>>
    where
        T: EntryReader<D>,
    {
        let projected = self.project();
        let filled_len_before = buf.filled().len();
        ready!(projected.inner_reader.poll_read(data, cx, buf))?;
        projected.hasher.update(&buf.filled()[filled_len_before..]);

        Poll::Ready(Ok(()))
    }
}

impl<D, T: EntryReader<D>> EntryReader<D> for CrcReader<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        data: &D,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<()>> {
        self.poll_read(data, cx, buf)
    }

    fn start_seek(self: Pin<&mut Self>, data: &D, pos: SeekFrom) -> Result<()> {
        let projected = self.project();
        *projected.did_seek = true;
        *projected.seek_in_progress = true;
        projected.inner_reader.start_seek(data, pos)
    }

    fn poll_seek_complete(
        self: Pin<&mut Self>,
        data: &D,
        cx: &mut Context<'_>,
    ) -> Poll<Result<u64>> {
        let projected = self.project();
        let pos = ready!(projected.inner_reader.poll_seek_complete(data, cx))?;
        *projected.seek_in_progress = false;
        Poll::Ready(Ok(pos))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::test_util::{entry_reader, read_size_strategy};
    use assert2::assert;
    use std::io::Cursor;
    use std::pin::pin;
    use test_strategy::proptest;

    #[proptest(async = "tokio")]
    async fn passes_through_data_and_crc(
        content: Vec<u8>,
        #[strategy(read_size_strategy())] read_size: usize,
    ) {
        let data = &content[..];
        let mut reader = pin!(CrcReader::new(Cursor::new(data)));
        let output = entry_reader::read_to_vec(reader.as_mut(), &data, read_size)
            .await
            .unwrap();

        assert!(output == content);
        assert!(reader.get_crc32() == crc32fast::hash(&content));
    }

    /// Verify a known example CRC value.
    /// The example is taken from [unit tests of crate Zip](https://github.com/zip-rs/zip/blob/75e8f6bab5a6525014f6f52c6eb608ab46de48af/src/crc32.rs#L77)
    #[tokio::test]
    async fn known_crc() {
        let data: &[u8] = b"1234";
        let mut reader = pin!(CrcReader::new(Cursor::new(data)));
        let _ = entry_reader::read_to_vec(reader.as_mut(), &data, data.len())
            .await
            .unwrap();
        assert!(reader.get_crc32() == 0x9be3_e0a3);
    }
}
