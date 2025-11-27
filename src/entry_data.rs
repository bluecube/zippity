use std::{
    future::{Future, Ready},
    io::{Cursor, Result, SeekFrom},
    pin::Pin,
    task::{Context, Poll},
};

use tokio::io::{AsyncRead, AsyncSeek, Empty, ReadBuf, empty};

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

impl EntryData for () {
    type Reader = Empty;
    type Future = Ready<Result<Self::Reader>>;

    fn get_reader(&self) -> Self::Future {
        std::future::ready(Ok(empty()))
    }

    fn size(&self) -> u64 {
        0
    }
}

impl<'a> EntryData for &'a [u8] {
    type Reader = Cursor<&'a [u8]>;
    type Future = Ready<Result<Self::Reader>>;

    fn get_reader(&self) -> Self::Future {
        std::future::ready(Ok(Cursor::new(self)))
    }

    fn size(&self) -> u64 {
        self.len() as u64
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
        let size = check_size_matches(&()).await;
        assert!(size == 0);
    }

    #[tokio::test]
    async fn slice_entry() {
        let entry = b"23456789sdfghjk,".as_slice();

        check_size_matches(&entry).await;

        let reader = pin!(entry.get_reader().await.unwrap());
        let read_back = entry_reader::read_to_vec(reader, &entry, 1024).await.unwrap();

        assert!(read_back.as_slice() == entry);
    }

}
