#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

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

#[derive(Debug, Error)]
pub enum Error {
    #[error("Entry name too long (length must fit into 16bit)")]
    TooLongEntryName { entry_name: String },
    #[error("Duplicate entry name {entry_name}")]
    DuplicateEntryName { entry_name: String },
    #[error("Entry {entry_name} reports size {expected_size} B, but was {actual_size} B")]
    SizeMismatch {
        entry_name: String,
        expected_size: u64,
        actual_size: u64,
    },
    #[error(
        "Entry {entry_name} was given a CRC value {expected_crc:08x} that does not match the computed {actual_crc:08x}"
    )]
    Crc32Mismatch {
        entry_name: String,
        expected_crc: u32,
        actual_crc: u32,
    },
    #[error("Attempting to seek before the start of the file")]
    SeekingBeforeStart,

    #[cfg(feature = "tokio-file")]
    #[error("Error reading symlink: {source}")]
    ReadlinkFailed { source: std::io::Error },

    #[cfg(feature = "tokio-file")]
    #[error("Error when traversig directories: {source}")]
    DirectoryTraversalFailed { source: std::io::Error },
}

impl From<Error> for std::io::Error {
    fn from(val: Error) -> Self {
        std::io::Error::other(val)
    }
}
