pub mod funky_entry_data;
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
        }
        size += read_size as u64;
    }
}

/// Helper functions for working with `EntryReader`
pub mod entry_reader {
    use std::io::Result;
    use std::{future::poll_fn, io::SeekFrom, pin::Pin};
    use tokio::io::ReadBuf;

    use crate::entry_data::EntryReader;

    /// Reads from an `EntryReader` into a buffer (similar to `AsyncReadExt::read`)
    pub async fn read<D, R: EntryReader<D>>(
        mut reader: Pin<&mut R>,
        data: &D,
        buf: &mut [u8],
    ) -> Result<usize> {
        let mut read_buf = ReadBuf::new(buf);
        poll_fn(|cx| reader.as_mut().poll_read(data, cx, &mut read_buf)).await?;
        Ok(read_buf.filled().len())
    }

    /// Reads exact bytes from an `EntryReader` (similar to `AsyncReadExt::read_exact`)
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

    /// Seeks within an `EntryReader` (similar to `AsyncSeekExt::seek`)
    pub async fn seek<D, R: EntryReader<D>>(
        mut reader: Pin<&mut R>,
        data: &D,
        pos: SeekFrom,
    ) -> Result<u64> {
        reader.as_mut().start_seek(data, pos)?;
        poll_fn(|cx| reader.as_mut().poll_seek_complete(data, cx)).await
    }

    /// Collects all data from an `EntryReader` to a `Vec`.
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

    /// Goes through all data from an `EntryReader`, discarding it,
    /// and returns the total number of bytes.
    pub async fn measure_size<D, R: EntryReader<D>>(reader: Pin<&mut R>, data: &D) -> Result<u64> {
        let vec = read_to_vec(reader, data, 8192).await?;
        Ok(vec.len() as u64)
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
    ZipArchive::new(std::io::Cursor::new(buf)).expect("Should be a valid ZIP")
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
/// Calculates skip length based on a original length and a fraction between 0 and 1.
/// Used for testing seek behavior.
/// Only used in tests, has possible accuracy issues (might not be able to reach every byte).
pub fn skip_length(len: usize, factor: f64) -> u64 {
    assert!((0.0..=1.0).contains(&factor));

    (len as f64 * factor) as u64
}
