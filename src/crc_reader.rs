use std::{
    io::{Result, SeekFrom},
    pin::Pin,
    task::{Context, Poll, ready},
};

use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncSeek, ReadBuf};

/// Wraps an existing AsyncRead and AsyncSeek object and calculates CRC32 while reading from it.
/// Also adapts the tokio AsyncSeek to something closer to std::futures::AsyncSeek
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

impl<T: AsyncRead> AsyncRead for CrcReader<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<()>> {
        let projected = self.project();
        let filled_len_before = buf.filled().len();
        ready!(projected.inner_reader.poll_read(cx, buf))?;
        projected.hasher.update(&buf.filled()[filled_len_before..]);

        Poll::Ready(Ok(()))
    }
}

impl<T: AsyncSeek> CrcReader<T> {
    pub fn seek(
        self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        position: SeekFrom,
    ) -> Poll<Result<u64>> {
        let mut projected = self.project();
        if !*projected.seek_in_progress {
            *projected.did_seek = true;
            *projected.seek_in_progress = true;
            projected.inner_reader.as_mut().start_seek(position)?;
        }

        let pos = ready!(projected.inner_reader.poll_complete(ctx))?;
        *projected.seek_in_progress = false;
        Poll::Ready(Ok(pos))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::test_util::{read_size_strategy, read_to_vec};
    use assert2::assert;
    use std::pin::pin;
    use test_strategy::proptest;

    #[proptest(async = "tokio")]
    async fn passes_through_data_and_crc(
        content: Vec<u8>,
        #[strategy(read_size_strategy())] read_size: usize,
    ) {
        let mut reader = pin!(CrcReader::new(&content[..]));
        let output = read_to_vec(reader.as_mut(), read_size).await.unwrap();

        assert!(output == content);
        assert!(reader.get_crc32() == crc32fast::hash(&content));
    }

    /// Verify a known example CRC value.
    /// The example is taken from unit tests from crate Zip:
    /// https://github.com/zip-rs/zip/blob/75e8f6bab5a6525014f6f52c6eb608ab46de48af/src/crc32.rs#L56
    #[tokio::test]
    async fn known_crc() {
        let data: &[u8] = b"1234";
        let mut reader = pin!(CrcReader::new(data));
        let _ = read_to_vec(reader.as_mut(), data.len()).await.unwrap();
        assert!(reader.get_crc32() == 0x9be3e0a3);
    }
}
