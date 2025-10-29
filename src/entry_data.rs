use std::{
    future::{Future, Ready},
    io::{Cursor, Result},
    pin::Pin,
    task::{Context, Poll, ready},
};

use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncSeek, Empty, empty};
use tokio_util::either::Either;

pub trait EntryData {
    type Reader: AsyncRead + AsyncSeek;
    type Future: Future<Output = Result<Self::Reader>>;

    /// Returns a future that when awaited will provide the reader for file data.
    fn get_reader(&self) -> Self::Future;

    /// Returns the size of the data of the entry, that will be read through get_reader.
    /// This is allowed to
    fn size(&self) -> u64;
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

impl<T: EntryData> EntryData for Option<T> {
    type Reader = Either<T::Reader, Empty>;
    type Future = OptionEntryDataFuture<T>;

    fn get_reader(&self) -> Self::Future {
        OptionEntryDataFuture {
            inner: self.as_ref().map(|ed| ed.get_reader()),
        }
    }

    fn size(&self) -> u64 {
        match self {
            Some(entry) => entry.size(),
            None => 0,
        }
    }
}

#[pin_project]
pub struct OptionEntryDataFuture<T: EntryData> {
    #[pin]
    inner: Option<T::Future>,
}

impl<T: EntryData> Future for OptionEntryDataFuture<T> {
    type Output = Result<Either<T::Reader, Empty>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project().inner.as_pin_mut() {
            Some(inner) => {
                let reader_result = ready!(inner.poll(cx))?;
                Poll::Ready(Ok(Either::Left(reader_result)))
            }
            None => Poll::Ready(Ok(Either::Right(empty()))),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::pin::pin;

    use assert2::assert;

    use crate::test_util::{measure_size, read_to_vec};

    use super::EntryData;

    pub(crate) async fn check_size_matches<T: EntryData>(entry: &T) -> u64 {
        let reader = pin!(entry.get_reader().await.unwrap());
        let expected_size = entry.size();
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

    #[tokio::test]
    async fn option_entry_none() {
        let size = check_size_matches::<Option<&[u8]>>(&None).await;
        assert!(size == 0);
    }

    #[tokio::test]
    async fn option_entry_some() {
        let value = b"23456789sdfghjk,".as_slice();
        let entry = Some(value);

        check_size_matches(&entry).await;

        let reader = pin!(entry.get_reader().await.unwrap());
        let read_back = read_to_vec(reader, 1024).await.unwrap();

        assert!(read_back.as_slice() == value);
    }
}
