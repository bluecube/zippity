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

pub struct TokioFileEntry(PathBuf);

impl TokioFileEntry {
    /// Construct a new file entry.
    pub fn new(path: PathBuf) -> TokioFileEntry {
        TokioFileEntry(path)
    }
}

impl From<PathBuf> for TokioFileEntry {
    fn from(path: PathBuf) -> Self {
        TokioFileEntry::new(path)
    }
}

impl EntryData for TokioFileEntry {
    // TODO: Here we're basically reimplementing File::open and metadata() from Tokio,
    // because we can't name its return type. Once `impl Trait` in associated types
    // becomes stable, we should convert to directly calling those.

    type SizeFuture = FileSizeFuture;
    type Reader = File;
    type ReaderFuture = FileReaderFuture;

    fn size(&self) -> FileSizeFuture {
        let path = self.0.clone();
        FileSizeFuture(spawn_blocking(move || Ok(std::fs::metadata(path)?.len())))
    }

    fn get_reader(&self) -> FileReaderFuture {
        let path = self.0.clone();
        FileReaderFuture(spawn_blocking(move || std::fs::File::open(path)))
    }
}

#[pin_project]
pub struct FileSizeFuture(#[pin] JoinHandle<Result<u64>>);

impl Future for FileSizeFuture {
    type Output = Result<u64>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let size = ready!(self.project().0.poll(cx)).map_err(|e| std::io::Error::other(e))??;
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
    async fn can_determine_file_size(content: Vec<u8>) {
        let (mut tempfile, tempfile_name) = tempfile::NamedTempFile::new().unwrap().into_parts();
        tempfile.write_all(content.as_slice()).unwrap();
        drop(tempfile);

        let entry = TokioFileEntry::new(tempfile_name.to_path_buf());

        assert!(entry.size().await.unwrap() == content.len() as u64);
    }

    #[proptest(async = "tokio")]
    async fn can_read_content(content: Vec<u8>) {
        let (mut tempfile, tempfile_name) = tempfile::NamedTempFile::new().unwrap().into_parts();
        tempfile.write_all(content.as_slice()).unwrap();
        drop(tempfile);

        let entry = TokioFileEntry::new(tempfile_name.to_path_buf());

        let reader = pin!(entry.get_reader().await.unwrap());

        assert!(read_to_vec(reader, 8192).await.unwrap() == content);
    }
}
