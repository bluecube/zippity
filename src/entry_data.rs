use std::{
    future::{Future, Ready},
    io::{Cursor, Result},
};

use tokio::io::{AsyncRead, AsyncSeek};

pub trait EntryData {
    type SizeFuture: Future<Output = Result<u64>>;
    type Reader: AsyncRead + AsyncSeek;
    type ReaderFuture: Future<Output = Result<Self::Reader>>;

    /// Returns the size of the data of the entry, that will be read through get_reader.
    fn size(&self) -> Self::SizeFuture;

    /// Returns a future that when awaited will provide the reader for file data.
    fn get_reader(&self) -> Self::ReaderFuture;
}

impl EntryData for () {
    type SizeFuture = std::future::Ready<Result<u64>>;
    type Reader = std::io::Cursor<&'static [u8]>;
    type ReaderFuture = std::future::Ready<Result<Self::Reader>>;

    fn size(&self) -> Self::SizeFuture {
        std::future::ready(Ok(0))
    }

    fn get_reader(&self) -> Self::ReaderFuture {
        std::future::ready(Ok(std::io::Cursor::new(&[])))
    }
}

impl<'a> EntryData for &'a [u8] {
    type SizeFuture = Ready<Result<u64>>;
    type Reader = Cursor<&'a [u8]>;
    type ReaderFuture = Ready<Result<Self::Reader>>;

    fn size(&self) -> Self::SizeFuture {
        std::future::ready(Ok(self.len() as u64))
    }

    fn get_reader(&self) -> Self::ReaderFuture {
        std::future::ready(Ok(Cursor::new(self)))
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
