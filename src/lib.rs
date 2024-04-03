#![forbid(unsafe_code)]

mod builder;
mod crc_reader;
mod entry_data;
mod error;
mod structs;
mod test_util;
mod zippity;

pub use builder::{Builder, BuilderEntry};
pub use entry_data::EntryData;
pub use error::ZippityError;
pub use zippity::Reader;

#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadMe;
