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
    #[error("Attempting to seek before the start of the file")]
    SeekingBeforeStart,
}
