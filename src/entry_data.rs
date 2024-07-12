use std::{
    future::{Future, Ready},
    io::{Cursor, Result},
};

use tokio::io::{AsyncRead, AsyncSeek};

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
    type Reader = std::io::Cursor<&'static [u8]>;
    type Future = std::future::Ready<Result<Self::Reader>>;

    fn get_reader(&self) -> Self::Future {
        std::future::ready(Ok(std::io::Cursor::new(&[])))
    }
}

impl EntrySize for () {
    type Future = std::future::Ready<Result<u64>>;

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

// impl<T: AsRef<U>, U: EntryData> EntryData for T {
//     type SizeFuture = U::SizeFuture;
//     type Reader = U::Reader;
//     type ReaderFuture = U::ReaderFuture;

//     fn size(&self) -> Self::SizeFuture {
//         self.as_ref().size()
//     }

//     fn get_reader(&self) -> Self::ReaderFuture {
//         self.as_ref().get_reader()
//     }
// }
