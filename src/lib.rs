#![doc = include_str!("../README.md")]

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

pub use builder::{AddEntryError, Builder, BuilderEntry};
pub use entry_data::EntryData;
pub use reader::{ReadError, Reader};

#[cfg(feature = "tokio-file")]
pub use filesystem_entry::FilesystemEntry;

#[cfg(feature = "bytes")]
pub use bytes::BytesStream;
