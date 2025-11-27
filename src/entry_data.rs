use std::{
    future::{Future, Ready},
    io::{Result, SeekFrom},
    pin::Pin,
    task::{Context, Poll},
};

use tokio::io::{AsyncRead, AsyncSeek, ReadBuf};

pub trait EntryData {
    type Reader: EntryReader<Self>;
    type Future: Future<Output = Result<Self::Reader>>;

    /// Returns a future that when awaited will provide the reader for file data.
    fn get_reader(&self) -> Self::Future;

    /// Returns the size of the data of the entry, that will be read through get_reader.
    /// This is allowed to
    fn size(&self) -> u64;
}

/// A trait for reading and seeking entry data.
/// Similar to AsyncRead + AsyncSeek but takes a reference to the entry data,
/// allowing readers to borrow data from the entry instead of requiring ownership.
pub trait EntryReader<D: ?Sized> {
    fn poll_read(
        self: Pin<&mut Self>,
        data: &D,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<()>>;

    fn start_seek(self: Pin<&mut Self>, data: &D, pos: SeekFrom) -> Result<()>;

    fn poll_seek_complete(
        self: Pin<&mut Self>,
        data: &D,
        cx: &mut Context<'_>,
    ) -> Poll<Result<u64>>;
}

/// Blanket implementation for types that already implement AsyncRead + AsyncSeek.
/// The data parameter is ignored since these types don't need it.
impl<D, T: AsyncRead + AsyncSeek> EntryReader<D> for T {
    fn poll_read(
        self: Pin<&mut Self>,
        _data: &D,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<()>> {
        AsyncRead::poll_read(self, cx, buf)
    }

    fn start_seek(self: Pin<&mut Self>, _data: &D, pos: SeekFrom) -> Result<()> {
        AsyncSeek::start_seek(self, pos)
    }

    fn poll_seek_complete(
        self: Pin<&mut Self>,
        _data: &D,
        cx: &mut Context<'_>,
    ) -> Poll<Result<u64>> {
        AsyncSeek::poll_complete(self, cx)
    }
}

/// A reader for types that implement AsRef<[u8]>.
/// Borrows data on each read via AsRef::as_ref().
/// Seeking behavior matches std::io::Cursor.
#[derive(Debug, Clone, Copy, Default)]
pub struct AsRefReader {
    position: usize,
}

impl<T: AsRef<[u8]>> EntryReader<T> for AsRefReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        data: &T,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<()>> {
        let slice = data.as_ref();

        // Use slice.get() for bounds checking - returns None if position is past the end
        let remaining = match slice.get(self.position..) {
            Some(remaining) => remaining,
            None => return Poll::Ready(Ok(())), // EOF
        };

        let to_read = remaining.len().min(buf.remaining());
        buf.put_slice(&remaining[..to_read]);
        self.position += to_read;

        Poll::Ready(Ok(()))
    }

    fn start_seek(mut self: Pin<&mut Self>, data: &T, pos: SeekFrom) -> Result<()> {
        let size = data.as_ref().len() as u64;

        let new_position = match pos {
            SeekFrom::Start(offset) => usize::try_from(offset).ok(),
            SeekFrom::End(offset) => size
                .checked_add_signed(offset)
                .and_then(|pos| usize::try_from(pos).ok()),
            SeekFrom::Current(offset) => (self.position as u64)
                .checked_add_signed(offset)
                .and_then(|pos| usize::try_from(pos).ok()),
        };

        self.position = new_position.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid seek to a negative or overflowing position",
            )
        })?;

        Ok(())
    }

    fn poll_seek_complete(
        self: Pin<&mut Self>,
        _data: &T,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<u64>> {
        Poll::Ready(Ok(self.position as u64))
    }
}

/// Blanket implementation for types that implement AsRef<[u8]>.
/// This covers &[u8], Vec<u8>, String, Box<[u8]>, Arc<[u8]>, Bytes, etc.
impl<T: AsRef<[u8]>> EntryData for T {
    type Reader = AsRefReader;
    type Future = Ready<Result<Self::Reader>>;

    fn get_reader(&self) -> Self::Future {
        std::future::ready(Ok(AsRefReader::default()))
    }

    fn size(&self) -> u64 {
        self.as_ref().len() as u64
    }
}

#[cfg(test)]
mod tests {
    use std::pin::pin;

    use assert2::assert;

    use super::EntryData;
    use crate::test_util::entry_reader;

    pub(crate) async fn check_size_matches<T: EntryData>(entry: &T) -> u64 {
        let reader = pin!(entry.get_reader().await.unwrap());
        let expected_size = entry.size();
        let actual_size = entry_reader::measure_size(reader, entry).await.unwrap();

        assert!(actual_size == expected_size);

        actual_size
    }

    #[tokio::test]
    async fn empty_entry() {
        let size = check_size_matches(&b"").await;
        assert!(size == 0);
    }

    #[tokio::test]
    async fn slice_entry() {
        let entry = b"23456789sdfghjk,".as_slice();

        check_size_matches(&entry).await;

        let reader = pin!(entry.get_reader().await.unwrap());
        let read_back = entry_reader::read_to_vec(reader, &entry, 1024)
            .await
            .unwrap();

        assert!(read_back.as_slice() == entry);
    }

    mod asref_reader_seeking {
        use std::{io::SeekFrom, pin::pin};

        use assert2::assert;

        use super::super::EntryData;
        use crate::test_util::entry_reader;

        #[tokio::test]
        async fn seek_past_end_and_read() {
            let data = b"hello";
            let mut reader = pin!(data.get_reader().await.unwrap());

            // Seek past the end
            let pos = entry_reader::seek(reader.as_mut(), &data, SeekFrom::Start(100))
                .await
                .unwrap();
            assert!(pos == 100);

            // Reading should return EOF (0 bytes)
            let mut buf = [0u8; 10];
            let n = entry_reader::read(reader.as_mut(), &data, &mut buf)
                .await
                .unwrap();
            assert!(n == 0);
        }

        #[tokio::test]
        async fn seek_negative_from_current() {
            let data = b"hello world";
            let mut reader = pin!(data.get_reader().await.unwrap());

            // Read some data first
            let mut buf = [0u8; 5];
            entry_reader::read_exact(reader.as_mut(), &data, &mut buf)
                .await
                .unwrap();
            assert!(buf == *b"hello");

            // Seek backward
            entry_reader::seek(reader.as_mut(), &data, SeekFrom::Current(-3))
                .await
                .unwrap();

            // Read again
            let mut buf2 = [0u8; 5];
            entry_reader::read_exact(reader.as_mut(), &data, &mut buf2)
                .await
                .unwrap();
            assert!(buf2 == *b"llo w");
        }

        #[tokio::test]
        async fn seek_from_end() {
            let data = b"hello world";
            let mut reader = pin!(data.get_reader().await.unwrap());

            // Seek to 5 bytes before end
            entry_reader::seek(reader.as_mut(), &data, SeekFrom::End(-5))
                .await
                .unwrap();

            let mut buf = [0u8; 5];
            entry_reader::read_exact(reader.as_mut(), &data, &mut buf)
                .await
                .unwrap();
            assert!(buf == *b"world");
        }

        #[tokio::test]
        async fn seek_to_negative_position_errors() {
            let data = b"hello";
            let mut reader = pin!(data.get_reader().await.unwrap());

            // Seek past end is ok
            let result = entry_reader::seek(reader.as_mut(), &data, SeekFrom::Start(10)).await;
            assert!(result.is_ok());

            // Try to seek to negative position from current
            let result = entry_reader::seek(reader.as_mut(), &data, SeekFrom::Current(-20)).await;
            assert!(result.is_err());
            assert!(result.unwrap_err().kind() == std::io::ErrorKind::InvalidInput);
        }

        #[tokio::test]
        async fn seek_overflow_from_end_errors() {
            let data = b"hello";
            let mut reader = pin!(data.get_reader().await.unwrap());

            // Try to overflow by seeking from end with i64::MIN
            let result = entry_reader::seek(reader.as_mut(), &data, SeekFrom::End(i64::MIN)).await;
            assert!(result.is_err());
            assert!(result.unwrap_err().kind() == std::io::ErrorKind::InvalidInput);
        }

        #[tokio::test]
        async fn multiple_seeks_and_reads() {
            let data = b"0123456789";
            let mut reader = pin!(data.get_reader().await.unwrap());

            // Read forward
            let mut buf = [0u8; 3];
            entry_reader::read_exact(reader.as_mut(), &data, &mut buf)
                .await
                .unwrap();
            assert!(buf == *b"012");

            // Seek to start
            entry_reader::seek(reader.as_mut(), &data, SeekFrom::Start(5))
                .await
                .unwrap();
            entry_reader::read_exact(reader.as_mut(), &data, &mut buf)
                .await
                .unwrap();
            assert!(buf == *b"567");

            // Seek to end
            entry_reader::seek(reader.as_mut(), &data, SeekFrom::End(-2))
                .await
                .unwrap();
            let mut buf2 = [0u8; 2];
            entry_reader::read_exact(reader.as_mut(), &data, &mut buf2)
                .await
                .unwrap();
            assert!(buf2 == *b"89");
        }

        #[tokio::test]
        async fn vec_u8_works() {
            let data = vec![1u8, 2, 3, 4, 5];
            let reader = pin!(data.get_reader().await.unwrap());
            let read_back = entry_reader::read_to_vec(reader, &data, 1024)
                .await
                .unwrap();
            assert!(read_back == data);
        }

        #[tokio::test]
        async fn string_works() {
            let data = String::from("hello world");
            let reader = pin!(data.get_reader().await.unwrap());
            let read_back = entry_reader::read_to_vec(reader, &data, 1024)
                .await
                .unwrap();
            assert!(read_back == data.as_bytes());
        }
    }
}
