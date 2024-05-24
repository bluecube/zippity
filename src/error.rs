use thiserror::Error;

#[derive(Clone, Debug, Error, PartialEq)]
pub enum ZippityError {
    #[error("Entry name too long (length must fit into 16bit)")]
    TooLongEntryName { entry_name: String },
    #[error("Duplicate entry name {entry_name}")]
    DuplicateEntryName { entry_name: String },
    #[error("Entry {entry_name} reports length {expected_size} B, but was {actual_size} B")]
    LengthMismatch {
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
}
