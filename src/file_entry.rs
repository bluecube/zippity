use crate::entry_data::EntryData;
use futures_util::future::TryFutureExt;
use std::{future::Future, io::Result, path::PathBuf, pin::Pin};
use tokio::fs::{metadata, File};

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
    // TODO: The boxed futures are ugly as ****.
    // Once `impl Trait` in associated types becomes stable,
    // we should convert to directly using those.

    type SizeFuture = Pin<Box<dyn Future<Output = Result<u64>>>>;
    type Reader = File;
    type ReaderFuture = Pin<Box<dyn Future<Output = Result<File>>>>;

    fn size(&self) -> Self::SizeFuture {
        Box::pin(metadata(self.0.clone()).map_ok(|m| m.len()))
    }

    fn get_reader(&self) -> Self::ReaderFuture {
        Box::pin(File::open(self.0.clone()))
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
