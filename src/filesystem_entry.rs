use std::{
    fs::Metadata,
    future::Future,
    io::Cursor,
    ops::DerefMut,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    task::{ready, Context, Poll},
};

use assert2::{assert, debug_assert};
use pin_project::pin_project;
use tokio::{
    fs::{read_dir, read_link, File},
    io::{AsyncRead, AsyncSeek, ReadBuf},
};

use crate::{Builder, BuilderEntry, EntryData, Error};

/// An EntryData implementation representing a filesystem object -- file, directory or symlink.
///
/// Constructing this structure directly through new() gives the most versatile interface, but
/// `Builder::add_filesystem_entry` or `Builder::add_directory_recursive` can be used as simpler
/// (and more opinionated) alternatives.
#[derive(Debug, Clone)]
pub struct FilesystemEntry {
    path: PathBuf,
    entry_type: EntryType,
}

#[derive(Debug, Clone)]
enum EntryType {
    File { size: u64 },
    Directory,
    Symlink { target_bytes: Arc<[u8]> },
}

impl FilesystemEntry {
    /// Constructs a new FilesystemEntry, with entry metadata given from outside.
    pub async fn with_metadata(path: PathBuf, metadata: &Metadata) -> Result<Self, Error> {
        let entry_type = EntryType::with_metadata(&path, metadata).await?;
        Ok(FilesystemEntry { path, entry_type })
    }
}

impl EntryType {
    async fn with_metadata(path: &Path, metadata: &Metadata) -> Result<Self, Error> {
        if metadata.is_dir() {
            Ok(EntryType::Directory)
        } else if metadata.is_symlink() {
            let target = read_link(path)
                .await
                .map_err(|e| Error::ReadlinkFailed { source: e })?;
            Ok(EntryType::Symlink {
                target_bytes: target.as_os_str().as_encoded_bytes().into(),
            })
        } else {
            Ok(EntryType::File {
                size: metadata.len(),
            })
        }
    }
}

impl EntryData for FilesystemEntry {
    type Reader = FilesystemEntryReader;
    type Future = FilesystemEntryFuture;

    fn get_reader(&self) -> Self::Future {
        match self.entry_type {
            EntryType::File { size: _ } => FilesystemEntryFuture::File {
                file_future: Box::pin(File::open(self.path.clone())),
            },
            EntryType::Symlink { ref target_bytes } => FilesystemEntryFuture::Symlink {
                target_bytes: Arc::clone(target_bytes),
            },
            EntryType::Directory => {
                unreachable!("Directories are zero-sized, should be skipped by reader")
            }
        }
    }

    fn size(&self) -> u64 {
        match self.entry_type {
            EntryType::File { size } => size,
            EntryType::Directory => 0,
            EntryType::Symlink { ref target_bytes } => target_bytes.len() as u64,
        }
    }
}

pub enum FilesystemEntryFuture {
    File {
        file_future: Pin<Box<dyn Future<Output = std::io::Result<File>>>>,
    },
    Symlink {
        target_bytes: Arc<[u8]>,
    },
    // Directory is not here, because zero sized entries are skipped by reader.
}

impl Future for FilesystemEntryFuture {
    type Output = std::io::Result<FilesystemEntryReader>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // FilesystemEntryFuture is Unpin, because it only contains the future through Pin<Box<_>>.
        match self.deref_mut() {
            FilesystemEntryFuture::File { file_future } => {
                let inner_reader = ready!(file_future.as_mut().poll(cx))?;
                Poll::Ready(Ok(FilesystemEntryReader::File(inner_reader)))
            }
            FilesystemEntryFuture::Symlink { target_bytes } => {
                let cursor = Cursor::new(Arc::clone(target_bytes));
                Poll::Ready(Ok(FilesystemEntryReader::Symlink(cursor)))
            }
        }
    }
}

#[pin_project(project = FilesystemEntryReaderProj)]
pub enum FilesystemEntryReader {
    File(#[pin] File),
    Symlink(#[pin] Cursor<Arc<[u8]>>),
    // Cursor is unpin, so we wouldn't really need to pin it here, but it saves a bit of typing later
}

impl AsyncRead for FilesystemEntryReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.project() {
            FilesystemEntryReaderProj::File(pin) => pin.poll_read(cx, buf),
            FilesystemEntryReaderProj::Symlink(cursor) => cursor.poll_read(cx, buf),
        }
    }
}

impl AsyncSeek for FilesystemEntryReader {
    fn start_seek(self: Pin<&mut Self>, position: std::io::SeekFrom) -> std::io::Result<()> {
        match self.project() {
            FilesystemEntryReaderProj::File(pin) => pin.start_seek(position),
            FilesystemEntryReaderProj::Symlink(cursor) => cursor.start_seek(position),
        }
    }

    fn poll_complete(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<u64>> {
        match self.project() {
            FilesystemEntryReaderProj::File(pin) => pin.poll_complete(cx),
            FilesystemEntryReaderProj::Symlink(cursor) => cursor.poll_complete(cx),
        }
    }
}

/// Modifies the entry_name to have no trailing slashes for files and one trailig slash for directories
fn sanitize_entry_name_slashes(mut entry_name: String, is_directory: bool) -> String {
    entry_name.truncate(entry_name.trim_end_matches('/').len());
    if is_directory {
        entry_name.push('/');
    }

    entry_name
}

/// Creates a string suitable as ZIP file entry name from a path.
///
/// `root` has to be a prefix of `path`.
/// `root_name` is used as described in `add_directory_recrusive`, but additionally it is not allowed to end with slashes.
/// If `is_directory`, then the generated path will end with a slash.
fn make_entry_name(
    path: &Path,
    root: &Path,
    root_name: Option<&str>,
    is_directory: bool,
) -> String {
    if let Some(root_name) = root_name {
        assert!(!root_name.ends_with('/'));
    }
    assert!(path != root);

    let root_separator = if root_name.is_some() { "/" } else { "" };
    let root_name = root_name.unwrap_or_default();
    let path = path
        .strip_prefix(root)
        .expect("`root` must be a prefix of `path`")
        .to_string_lossy();
    assert!(!path.ends_with('/'));
    debug_assert!(!path.is_empty());
    let trailing_slash = if is_directory { "/" } else { "" };

    format!("{}{}{}{}", root_name, root_separator, path, trailing_slash)
}

impl Builder<FilesystemEntry> {
    /// Addd a filesystem entry to the builder.
    /// This method handles setting entry type, permissions and modification time from the metadata
    /// and modifies the entry name to include a final slash for directories (or remove the slash for non-directories).
    /// Symlinks are added as symlink type, not followed.
    pub async fn add_filesystem_entry(
        &mut self,
        name: String,
        path: PathBuf,
        metadata: &Metadata,
    ) -> std::result::Result<&mut BuilderEntry<FilesystemEntry>, Error> {
        let fs_entry = FilesystemEntry::with_metadata(path, metadata).await?;
        let name =
            sanitize_entry_name_slashes(name, matches!(fs_entry.entry_type, EntryType::Directory));

        let added_entry = self.add_entry(name, fs_entry)?;
        added_entry.metadata(metadata);
        Ok(added_entry)
    }

    /// Adds content of a directory to the builder recursively.
    /// Adds both files and directories, calls `Builder::add_filesystem_entry` for each item.
    /// If `root_name` is Some, it is used as a prefix for all entry names, separated by a slash
    /// and also the root directory is added as a separate directory entry.
    /// If `root_name` is Some and contains slashes itself, its parent directories are not added as zip entries.
    pub async fn add_directory_recursive(
        &mut self,
        directory: PathBuf,
        root_name: Option<&str>,
    ) -> std::result::Result<(), Error> {
        let mut stack = vec![directory.clone()];
        let root_name_trimmed = root_name.map(|root_name| root_name.trim_end_matches('/'));

        let dtf = |e| Error::DirectoryTraversalFailed { source: e };

        while let Some(path) = stack.pop() {
            let mut dir = read_dir(path).await.map_err(dtf)?;

            while let Some(dir_entry) = dir.next_entry().await.map_err(dtf)? {
                let path = dir_entry.path();
                let metadata = dir_entry.metadata().await.map_err(dtf)?;

                if metadata.is_dir() {
                    stack.push(path.clone());
                }

                let entry_name =
                    make_entry_name(&path, &directory, root_name_trimmed, metadata.is_dir());
                let zip_entry = FilesystemEntry::with_metadata(path, &metadata).await?;

                let added_entry = self.add_entry(entry_name, zip_entry)?;
                added_entry.metadata(&metadata);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use std::ffi::OsStr;

    use super::*;

    use crate::proptest::{ArbitraryTestEntryDataParams, TestEntryData};

    use assert2::assert;
    use tempfile::TempDir;
    use test_case::test_case;
    use test_strategy::proptest;

    #[test_case("file.txt", false => "file.txt"; "file without trailing slash")]
    #[test_case("file.txt/", false => "file.txt"; "file with trailing slash")]
    #[test_case("directory", true => "directory/"; "directory without trailing slash")]
    #[test_case("directory/", true => "directory/"; "directory with trailing slash")]
    #[test_case("directory///", true => "directory/"; "directory with multiple trailing slashes")]
    #[test_case("a/b.txt", false => "a/b.txt"; "path with internal slashes")]
    #[test_case("/a///b.txt", false => "/a///b.txt"; "path with weird internal slashes is not modified")]
    fn test_sanitize_entry_name_slashes(entry_name: &str, is_directory: bool) -> String {
        sanitize_entry_name_slashes(entry_name.to_string(), is_directory)
    }

    #[test_case("/root/subdir/file.txt", "/root", None, false => "subdir/file.txt"; "file without root name")]
    #[test_case("/root/subdir", "/root", None, true => "subdir/"; "directory without root name")]
    #[test_case("/root/subdir/file.txt", "/root", Some("archive"), false => "archive/subdir/file.txt"; "file with root name")]
    #[test_case("/root/subdir", "/root", Some("archive"), true => "archive/subdir/"; "directory with root name")]
    #[test_case("/root/subdir", "/root/", Some("archive"), true => "archive/subdir/"; "root name with trailing slash")]
    fn test_make_entry_name(
        path: &str,
        root: &str,
        root_name: Option<&str>,
        is_directory: bool,
    ) -> String {
        let path = Path::new(path);
        let root = Path::new(root);
        make_entry_name(path, root, root_name, is_directory)
    }

    /// Tests that when adding a directory, entries are added to the builder as expected:
    /// Each file has an entry, each parent directory of the entry has an entry.
    /// Content is not tested at all.
    #[proptest(async = "tokio")]
    async fn add_directory_recursive_entries_no_prefix(
        #[any(ArbitraryTestEntryDataParams {
            entry_name_pattern: "[a-z]+(/[a-z]+)+", // limit special characters to not mess up the paths
            max_size: 0,
            ..Default::default()
        })]
        content: TestEntryData,
    ) {
        let test_dir = content.make_directory().unwrap();
        let mut builder = Builder::new();
        builder
            .add_directory_recursive(test_dir.as_ref().to_path_buf(), None)
            .await
            .unwrap();

        dbg!(content.0.len());

        for (name, _) in content.0 {
            // First check that the file entry itself is stored in the builder
            assert!(builder.entries.contains_key(name.as_str()));

            // Then check that every parent directory of the path is stored
            let mut path = name.as_str();
            loop {
                let Some(pos) = path.rfind('/') else { break };
                let path_with_slash = &path[..=pos];
                path = &path[..pos];
                assert!(builder.entries.contains_key(path_with_slash));
            }
        }
    }

    #[tokio::test]
    async fn symlink_entry_with_metadata() {
        let test_dir = TempDir::new().unwrap();

        let target_path = test_dir.path().join("target.txt");
        tokio::fs::write(&target_path, b"hello").await.unwrap();

        let symlink_path = test_dir.path().join("link.txt");

        std::os::unix::fs::symlink(&target_path, &symlink_path).unwrap();

        let metadata = tokio::fs::symlink_metadata(&symlink_path).await.unwrap();
        let fs_entry = FilesystemEntry::with_metadata(symlink_path.clone(), &metadata)
            .await
            .unwrap();

        match fs_entry.entry_type {
            EntryType::Symlink { ref target_bytes } => {
                use std::os::unix::ffi::OsStrExt;
                let resolved = Path::new(&OsStr::from_bytes(target_bytes)).to_path_buf();
                assert!(resolved.ends_with("target.txt"));
            }
            _ => panic!("Expected symlink entry type"),
        }

        assert_eq!(fs_entry.path, symlink_path);
    }

    // TODO: File entry, directory entry
}
