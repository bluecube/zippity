#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

mod builder;
mod crc_reader;
mod entry_data;
mod error;
mod reader;
mod structs;

#[cfg(feature = "tokio-file")]
mod tokio_file;

#[cfg(test)]
mod test_util;

pub use builder::{Builder, BuilderEntry};
pub use entry_data::EntryData;
pub use error::ZippityError;
pub use reader::Reader;
