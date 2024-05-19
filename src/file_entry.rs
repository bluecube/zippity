use std::{
    future::Future,
    io::Result,
    path::PathBuf,
    pin::Pin,
    task::{ready, Context, Poll},
};

use pin_project::pin_project;
use tokio::{
    fs::File,
    task::{spawn_blocking, JoinHandle},
};

use crate::entry_data::EntryData;

pub struct FileEntry {
    path: PathBuf,
    size: Option<u64>,
}

impl FileEntry {
    /// Construct a new file entry.
    /// File will be later queried for metadata to get its size.
    pub fn new(path: PathBuf) -> FileEntry {
        FileEntry { path, size: None }
    }

    /// Construct a new file entry, giving its metadata in advance.
    pub fn with_size(path: PathBuf, size: u64) -> FileEntry {
        FileEntry {
            path,
            size: Some(size),
        }
    }
}

impl EntryData for FileEntry {
    // TODO: Here we're basically reimplementing File::open and metadata() from Tokio,
    // because we can't name its return type. Once `impl Trait` in associated types
    // becomes stable, we should convert to directly calling those.

    type SizeFuture = FileSizeFuture;
    type Reader = File;
    type ReaderFuture = FileReaderFuture;

    fn size(&self) -> FileSizeFuture {
        if let Some(size) = self.size {
            FileSizeFuture::KnowInAdvance(size)
        } else {
            let path = self.path.clone();
            FileSizeFuture::WaitingForMetadata(spawn_blocking(move || {
                Ok(std::fs::metadata(path)?.len())
            }))
        }
    }

    fn get_reader(&self) -> FileReaderFuture {
        let path = self.path.clone();
        FileReaderFuture(spawn_blocking(move || std::fs::File::open(path)))
    }
}

#[pin_project(project = FileSizeFutureProj)]
pub enum FileSizeFuture {
    KnowInAdvance(u64),
    WaitingForMetadata(#[pin] JoinHandle<Result<u64>>),
}

impl Future for FileSizeFuture {
    type Output = Result<u64>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let size = match self.project() {
            FileSizeFutureProj::KnowInAdvance(size) => *size,
            FileSizeFutureProj::WaitingForMetadata(handle) => {
                ready!(handle.poll(cx)).map_err(|e| std::io::Error::other(e))??
            }
        };
        Poll::Ready(Ok(size))
    }
}

#[pin_project]
pub struct FileReaderFuture(#[pin] JoinHandle<Result<std::fs::File>>);

impl Future for FileReaderFuture {
    type Output = Result<File>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let std = ready!(self.project().0.poll(cx)).map_err(|e| std::io::Error::other(e))??;
        Poll::Ready(Ok(File::from_std(std)))
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use std::io::Write;
    use std::pin::pin;

    use crate::test_util::read_to_vec;
    use test_strategy::proptest;

    #[proptest(async = "tokio")]
    async fn with_size_returns_given_size(size: u64) {
        let entry = FileEntry::with_size("nonexistent path".into(), size);

        assert!(entry.size().await.unwrap() == size);
    }

    #[proptest(async = "tokio")]
    async fn can_determine_file_size(content: Vec<u8>) {
        let (mut tempfile, tempfile_name) = tempfile::NamedTempFile::new().unwrap().into_parts();
        tempfile.write_all(content.as_slice()).unwrap();
        drop(tempfile);

        let entry = FileEntry::new(tempfile_name.to_path_buf());

        assert!(entry.size().await.unwrap() == content.len() as u64);
    }

    #[proptest(async = "tokio")]
    async fn can_read_content(content: Vec<u8>) {
        let (mut tempfile, tempfile_name) = tempfile::NamedTempFile::new().unwrap().into_parts();
        tempfile.write_all(content.as_slice()).unwrap();
        drop(tempfile);

        let entry = FileEntry::new(tempfile_name.to_path_buf());

        let reader = pin!(entry.get_reader().await.unwrap());

        assert!(read_to_vec(reader, 8192).await.unwrap() == content);
    }
}
