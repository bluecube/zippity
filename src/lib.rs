mod crc_reader;
mod entry_data;
mod structs;
mod test_util;
mod zippity;
mod zippity_error;

pub use entry_data::EntryData;
pub use zippity::{Builder, Reader};
pub use zippity_error::ZippityError;

#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadMe;
