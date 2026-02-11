#![doc = include_str!("../README.md")]

use thiserror::Error;

mod builder;
mod crc_reader;
mod entry_data;
mod reader;
mod structs;

#[cfg(feature = "actix-web")]
mod actix_web;
#[cfg(feature = "bytes")]
mod bytes;
#[cfg(feature = "tokio-file")]
mod filesystem_entry;

#[cfg(test)]
mod integration_tests;
#[cfg(test)]
mod test_util;

pub use builder::{Builder, BuilderEntry};
pub use entry_data::EntryData;
pub use reader::Reader;

#[cfg(feature = "tokio-file")]
pub use filesystem_entry::FilesystemEntry;

#[cfg(feature = "bytes")]
pub use bytes::BytesStream;

// TODO: Split this error type!
/// Represents an error that can occur during ZIP file creation or reading.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// Entry name too long (length must fit into 16bit)
    #[error("Entry name too long")] // Not mentioning the name here, because it's too long :)
    TooLongEntryName {
        /// The name of the entry that was too long.
        entry_name: String,
    },
    /// Adding entry would cause the ZIP file to be longer than `u64::MAX`
    #[error("Adding entry {entry_name} would cause the ZIP file to be longer than u64::MAX")]
    TooLongZipFile {
        /// The name of the entry that would make the zip file too long.
        entry_name: String,
    },
    /// Duplicate entry name
    #[error("Duplicate entry name {entry_name}")]
    DuplicateEntryName {
        /// The name of the entry that was duplicated.
        entry_name: String,
    },
    /// Entry reports a size that does not match the actual size.
    #[error("Entry {entry_name} reports size {expected_size} B, but was {actual_size} B")]
    SizeMismatch {
        /// The name of the entry.
        entry_name: String,
        /// The expected size of the entry.
        expected_size: u64,
        /// The actual size of the entry.
        actual_size: u64,
    },
    /// Entry was given a CRC value that does not match the computed one.
    #[error(
        "Entry {entry_name} was given a CRC value {expected_crc:08x} that does not match the computed {actual_crc:08x}"
    )]
    Crc32Mismatch {
        /// The name of the entry.
        entry_name: String,
        /// The expected CRC32 value.
        expected_crc: u32,
        /// The actual CRC32 value.
        actual_crc: u32,
    },
    /// Attempting to seek before the start of the file.
    #[error("Attempting to seek before the start of the file")]
    SeekingBeforeStart,

    /// Error reading symlink.
    #[cfg(feature = "tokio-file")]
    #[error("Error reading symlink: {source}")]
    ReadlinkFailed {
        /// The underlying IO error.
        source: std::io::Error,
    },

    /// Error when traversing directories.
    #[cfg(feature = "tokio-file")]
    #[error("Error when traversig directories: {source}")]
    DirectoryTraversalFailed {
        /// The underlying IO error.
        source: std::io::Error,
    },
}

impl From<Error> for std::io::Error {
    fn from(val: Error) -> Self {
        std::io::Error::other(val)
    }
}
