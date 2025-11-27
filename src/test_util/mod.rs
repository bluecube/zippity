pub mod test_entry_data;

use assert2::assert;
use proptest::strategy::Strategy;
use std::pin::{Pin, pin};
use std::{fs, io::Result};
use tempfile::TempDir;
use tokio::io::{AsyncRead, AsyncReadExt};
use zip::ZipArchive;

use crate::{Builder, EntryData};

/// Returns a proptest strategy that minimizes to maximum read size
pub fn read_size_strategy() -> impl Strategy<Value = usize> {
    const MIN: usize = 1;
    const MAX: usize = 8192;
    (MIN..=MAX).prop_map(|v| MAX + MIN - v)
}

/// Takes an async readable, collects all data to vec.
/// Size of each read can be specified
pub async fn read_to_vec(
    mut reader: Pin<&mut impl AsyncRead>,
    read_size: usize,
) -> Result<Vec<u8>> {
    let mut buffer = Vec::new();

    loop {
        let size_before = buffer.len();
        buffer.resize(size_before + read_size, 0);
        let write_slice = &mut buffer[size_before..];
        assert!(write_slice.len() == read_size);

        let size_read = reader.read(write_slice).await?;

        buffer.truncate(size_before + size_read);

        if size_read == 0 {
            return Ok(buffer);
        }
    }
}

/// Creates a temp directory that contains a file a directory and a symlink.
#[cfg(unix)] // Uses unix symlinks
pub fn prepare_test_dir() -> TempDir {
    let tempdir = TempDir::new().unwrap();

    let dir_path = tempdir.path().join("dir");
    fs::create_dir(&dir_path).unwrap();
    let f_path = dir_path.join("file");
    fs::write(&f_path, b"Hello world").unwrap();
    std::os::unix::fs::symlink("dir/file", tempdir.path().join("link1")).unwrap();
    std::os::unix::fs::symlink("/foo/bar", tempdir.path().join("link2")).unwrap();

    tempdir
}

/// Takes an async readable, goes through all its data discarding it,
/// returns the total number of bytes.
pub async fn measure_size(mut reader: Pin<&mut impl AsyncRead>) -> Result<u64> {
    let mut buffer = vec![0; 8192];
    let mut size = 0;

    loop {
        let read_size = reader.read(buffer.as_mut_slice()).await?;
        if read_size == 0 {
            return Ok(size);
        } else {
            size += read_size as u64;
        }
    }
}

/// Helper functions for working with EntryReader
pub mod entry_reader {
    use std::io::Result;
    use std::{future::poll_fn, io::SeekFrom, pin::Pin};
    use tokio::io::ReadBuf;

    use crate::entry_data::EntryReader;

    /// Reads from an EntryReader into a buffer (similar to AsyncReadExt::read)
    pub async fn read<D, R: EntryReader<D>>(
        mut reader: Pin<&mut R>,
        data: &D,
        buf: &mut [u8],
    ) -> Result<usize> {
        let mut read_buf = ReadBuf::new(buf);
        poll_fn(|cx| reader.as_mut().poll_read(data, cx, &mut read_buf)).await?;
        Ok(read_buf.filled().len())
    }

    /// Reads exact bytes from an EntryReader (similar to AsyncReadExt::read_exact)
    pub async fn read_exact<D, R: EntryReader<D>>(
        mut reader: Pin<&mut R>,
        data: &D,
        buf: &mut [u8],
    ) -> Result<()> {
        let mut filled = 0;
        while filled < buf.len() {
            let n = read(reader.as_mut(), data, &mut buf[filled..]).await?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "failed to fill whole buffer",
                ));
            }
            filled += n;
        }
        Ok(())
    }

    /// Seeks within an EntryReader (similar to AsyncSeekExt::seek)
    pub async fn seek<D, R: EntryReader<D>>(
        mut reader: Pin<&mut R>,
        data: &D,
        pos: SeekFrom,
    ) -> Result<u64> {
        reader.as_mut().start_seek(data, pos)?;
        poll_fn(|cx| reader.as_mut().poll_seek_complete(data, cx)).await
    }

    /// Collects all data from an EntryReader to a Vec.
    /// Size of each read can be specified.
    pub async fn read_to_vec<D, R: EntryReader<D>>(
        mut reader: Pin<&mut R>,
        data: &D,
        read_size: usize,
    ) -> Result<Vec<u8>> {
        let mut buffer = Vec::new();

        loop {
            let size_before = buffer.len();
            buffer.resize(size_before + read_size, 0);
            let write_slice = &mut buffer[size_before..];

            let size_read = read(reader.as_mut(), data, write_slice).await?;

            buffer.truncate(size_before + size_read);

            if size_read == 0 {
                return Ok(buffer);
            }
        }
    }

    /// Goes through all data from an EntryReader, discarding it,
    /// and returns the total number of bytes.
    pub async fn measure_size<D, R: EntryReader<D>>(reader: Pin<&mut R>, data: &D) -> Result<u64> {
        let vec = read_to_vec(reader, data, 8192).await?;
        Ok(vec.len() as u64)
    }
}

pub mod funky_entry_data {
    use pin_project::pin_project;
    use std::{
        future::Future,
        io::{Cursor, Result},
        pin::Pin,
        task::Poll,
    };
    use tokio::io::{AsyncRead, AsyncSeek};

    use crate::entry_data::EntryData;

    /// Async readable that returns zeros.
    #[derive(Clone, Debug)]
    #[pin_project]
    pub struct Zeros {
        size: u64,
        remaining: u64,
    }

    /// EntryData implementation (+ its own reader), that reads only zeros
    impl Zeros {
        pub fn new(size: u64) -> Self {
            Zeros {
                size,
                remaining: size,
            }
        }
    }

    impl AsyncRead for Zeros {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            if self.remaining > 0 {
                let n = self.remaining.min(buf.remaining() as u64);
                buf.initialize_unfilled_to(n as usize).fill(0);
                buf.advance(n as usize);
                *self.project().remaining -= n;
            }
            Poll::Ready(Ok(()))
        }
    }

    impl AsyncSeek for Zeros {
        fn start_seek(self: Pin<&mut Self>, position: std::io::SeekFrom) -> std::io::Result<()> {
            match position {
                std::io::SeekFrom::Start(pos) => *self.project().remaining = self.size - pos,
                std::io::SeekFrom::End(_) => unimplemented!(),
                std::io::SeekFrom::Current(_) => unimplemented!(),
            };
            Ok(())
        }

        fn poll_complete(
            self: Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> Poll<std::io::Result<u64>> {
            Poll::Ready(Ok(self.size - self.remaining))
        }
    }

    impl EntryData for Zeros {
        type Reader = Self;
        type Future = std::future::Ready<Result<Self>>;

        fn get_reader(&self) -> Self::Future {
            std::future::ready(Ok(self.clone()))
        }

        fn size(&self) -> u64 {
            self.size
        }
    }

    /// EntryData implementation (+ its own reader) that provides data from a u8 slice,
    /// but alternates returning pending and ready on all futures calls.
    #[pin_project]
    pub struct LazyReader<'a> {
        #[pin]
        inner: Cursor<&'a [u8]>,
        delay: bool,
    }

    impl<'a> From<&'a [u8]> for LazyReader<'a> {
        fn from(value: &'a [u8]) -> Self {
            LazyReader {
                inner: Cursor::new(value),
                delay: true,
            }
        }
    }

    impl AsyncRead for LazyReader<'_> {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            let projected = self.project();

            *projected.delay = !*projected.delay;
            if !*projected.delay {
                cx.waker().wake_by_ref();
                Poll::Pending
            } else {
                projected.inner.poll_read(cx, buf)
            }
        }
    }

    impl AsyncSeek for LazyReader<'_> {
        fn start_seek(self: Pin<&mut Self>, position: std::io::SeekFrom) -> std::io::Result<()> {
            self.project().inner.start_seek(position)
        }

        fn poll_complete(
            self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> Poll<std::io::Result<u64>> {
            let projected = self.project();

            *projected.delay = !*projected.delay;
            if !*projected.delay {
                cx.waker().wake_by_ref();
                Poll::Pending
            } else {
                *projected.delay = true;
                projected.inner.poll_complete(cx)
            }
        }
    }

    pub struct LazyReaderFuture<'a> {
        data: &'a [u8],
        delay: bool,
    }

    impl<'a> Future for LazyReaderFuture<'a> {
        type Output = std::io::Result<LazyReader<'a>>;

        fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
            self.delay = !self.delay;
            if !self.delay {
                cx.waker().wake_by_ref();
                Poll::Pending
            } else {
                Poll::Ready(Result::Ok(LazyReader {
                    inner: Cursor::new(self.data),
                    delay: true,
                }))
            }
        }
    }

    impl<'a> EntryData for LazyReader<'a> {
        type Reader = LazyReader<'a>;
        type Future = LazyReaderFuture<'a>;

        fn get_reader(&self) -> Self::Future {
            LazyReaderFuture {
                data: self.inner.get_ref(),
                delay: true,
            }
        }

        fn size(&self) -> u64 {
            self.inner.get_ref().len() as u64
        }
    }

    /// Struct that reports some data size, but provides different.
    /// Returned data is all zeros.
    pub struct BadSize {
        pub reported_size: u64,
        pub actual_size: u64,
    }

    impl EntryData for BadSize {
        type Reader = Zeros;
        type Future = std::future::Ready<Result<Self::Reader>>;

        fn get_reader(&self) -> Self::Future {
            std::future::ready(Ok(Zeros::new(self.actual_size)))
        }

        fn size(&self) -> u64 {
            self.reported_size
        }
    }

    /// Provides zero size and panics when attempting to create the reader.
    pub struct EmptyUnsupportedReader();

    impl EntryData for EmptyUnsupportedReader {
        type Reader = std::io::Cursor<&'static [u8]>;
        type Future = std::future::Ready<Result<Self::Reader>>;

        fn get_reader(&self) -> Self::Future {
            unimplemented!("This test struct doesn't support getting futures")
        }

        fn size(&self) -> u64 {
            0
        }
    }
}

pub async fn build_and_open<T: EntryData>(
    builder: Builder<T>,
) -> ZipArchive<std::io::Cursor<Vec<u8>>> {
    let mut zippity = pin!(builder.build());
    let size = zippity.size();

    let mut buf = Vec::new();
    zippity.read_to_end(&mut buf).await.unwrap();

    assert!(size == (buf.len() as u64));
    ZipArchive::new(std::io::Cursor::new(buf)).expect("Should be a valid zip")
}
