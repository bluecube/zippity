use std::{
    future::{ready, Future, Ready},
    io::{Cursor, Result},
    pin::Pin,
    task::{ready, Context, Poll},
};

use pin_project::pin_project;
use tokio::io::{empty, AsyncRead, AsyncSeek, Empty};
use tokio_util::either::Either;

pub trait EntryData {
    type Reader: AsyncRead + AsyncSeek;
    type Future: Future<Output = Result<Self::Reader>>;

    /// Returns a future that when awaited will provide the reader for file data.
    fn get_reader(&self) -> Self::Future;
}

pub trait EntrySize {
    type Future: Future<Output = Result<u64>>;

    /// Returns the size of the data of the entry, that will be read through get_reader.
    fn size(&self) -> Self::Future;
}

impl EntryData for () {
    type Reader = Empty;
    type Future = Ready<Result<Self::Reader>>;

    fn get_reader(&self) -> Self::Future {
        std::future::ready(Ok(empty()))
    }
}

impl EntrySize for () {
    type Future = Ready<Result<u64>>;

    fn size(&self) -> Self::Future {
        std::future::ready(Ok(0))
    }
}

impl<'a> EntryData for &'a [u8] {
    type Reader = Cursor<&'a [u8]>;
    type Future = Ready<Result<Self::Reader>>;

    fn get_reader(&self) -> Self::Future {
        std::future::ready(Ok(Cursor::new(self)))
    }
}

impl<'a> EntrySize for &'a [u8] {
    type Future = Ready<Result<u64>>;

    fn size(&self) -> Self::Future {
        std::future::ready(Ok(self.len() as u64))
    }
}

#[cfg(test)]
mod tests {
    use std::pin::pin;

    use assert2::assert;

    use crate::test_util::{measure_size, read_to_vec};

    use super::{EntryData, EntrySize};

    pub(crate) async fn check_size_matches<T: EntryData + EntrySize>(entry: &T) -> u64 {
        let reader = pin!(entry.get_reader().await.unwrap());
        let expected_size = entry.size().await.unwrap();
        let actual_size = measure_size(reader).await.unwrap();

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
        let read_back = read_to_vec(reader, 1024).await.unwrap();

        assert!(read_back.as_slice() == entry);
    }
}
