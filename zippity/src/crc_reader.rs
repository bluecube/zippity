use std::{
    io::Result,
    pin::Pin,
    task::{ready, Context, Poll},
};

use pin_project::pin_project;
use tokio::io::{AsyncRead, ReadBuf};

/// Wraps an existing AsyncRead object and calculates CRC32 while reading from it.
#[pin_project]
pub struct CrcReader<T> {
    #[pin]
    inner_reader: T,
    hasher: crc32fast::Hasher,
}

impl<T> CrcReader<T> {
    pub fn new(inner_reader: T) -> Self {
        CrcReader {
            inner_reader,
            hasher: crc32fast::Hasher::new(),
        }
    }

    pub fn get_crc32(&self) -> u32 {
        // Cloning as a workaround -- finalize consumes, but we only have the hasher borrowed
        self.hasher.clone().finalize()
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
#[cfg(test)]
mod test {
    use std::pin::pin;

    use super::*;
    use crate::test_util::{read_size_strategy, read_to_vec, unasync};
    use assert2::assert;
    use test_strategy::proptest;

    #[proptest]
    fn passes_through_data_and_crc(
        content: Vec<u8>,
        #[strategy(read_size_strategy())] read_size: usize,
    ) {
        let mut reader = pin!(CrcReader::new(&content[..]));
        let output = unasync(read_to_vec(reader.as_mut(), read_size)).unwrap();

        assert!(output == content);
        assert!(reader.get_crc32() == crc32fast::hash(&content));
    }

    /// Verify a known example CRC value.
    /// The example is taken from unit tests from crate Zip:
    /// https://github.com/zip-rs/zip/blob/75e8f6bab5a6525014f6f52c6eb608ab46de48af/src/crc32.rs#L56
    #[test]
    fn known_crc() {
        let data: &[u8] = b"1234";
        let mut reader = pin!(CrcReader::new(data));
        let _ = unasync(read_to_vec(reader.as_mut(), data.len())).unwrap();
        assert!(reader.get_crc32() == 0x9be3e0a3);
    }
}
