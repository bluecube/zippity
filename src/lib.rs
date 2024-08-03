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
#[cfg(feature = "tokio-file")]
mod tokio_file;

#[cfg(feature = "bytes")]
mod bytes;

#[cfg(feature = "proptest")]
pub mod proptest;

#[cfg(test)]
mod test_util;

pub use builder::{Builder, BuilderEntry};
pub use entry_data::{EntryData, EntrySize};
pub use reader::Reader;

#[cfg(feature = "tokio-file")]
pub use tokio_file::TokioFileEntry;

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
    #[error("Entry {entry_name} was given a CRC value {expected_crc:08x} that does not match the computed {actual_crc:08x}")]
    Crc32Mismatch {
        entry_name: String,
        expected_crc: u32,
        actual_crc: u32,
    },
    #[error("Attempting to seek before the start of the file")]
    SeekingBeforeStart,
    #[error("Error getting entry size")]
    GettingEntrySize(#[from] std::io::Error),
}

impl From<Error> for std::io::Error {
    fn from(val: Error) -> Self {
        std::io::Error::other(val)
    }
}
