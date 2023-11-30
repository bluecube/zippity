use packed_struct::prelude::*;

/// Minimum version needed to extract the zip64 extensions required by zippity
const ZIP64_VERSION_TO_EXTRACT: u16 = 45;

/// Local file header
/// Preceedes every file.
/// Must be followed by file name nad extra fields (length is part of this struct)
#[derive(Debug, PackedStruct)]
#[packed_struct(endian = "lsb")]
pub struct LocalFileHeader {
    pub signature: u32,
    pub version_to_extract: u16,
    pub flags: u16,
    pub compression: u16,
    pub last_mod_time: u16,
    pub last_mod_date: u16,
    pub crc32: u32,
    pub compressed_size: u32,
    pub uncompressed_size: u32,
    pub file_name_len: u16,
    pub extra_field_len: u16,
}

impl LocalFileHeader {
    const LOCAL_FILE_HEADER_SIGNATURE: u32 = 0x04034b50;
}

#[derive(Debug, PackedStruct)]
#[packed_struct(endian = "lsb")]
pub struct Zip64ExtraField {
    pub crc32: u32,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
}

/// Zip64 version of the data descriptor
/// Follows file data.
#[derive(Debug, PackedStruct)]
#[packed_struct(endian = "lsb")]
pub struct DataDescriptor64 {
    pub crc32: u32,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
}

/// Central directory header
/// On per each file, placed in central directory.
#[derive(Debug, PackedStruct)]
#[packed_struct(endian = "lsb")]
pub struct CentralDirectoryHeader {
    pub TODO: u8,
}

#[derive(Debug, PackedStruct)]
#[packed_struct(endian = "lsb")]
pub struct EndOfCentralDirectory {
    pub signature: u32,
    pub this_disk_number: u16,
    pub start_of_cd_disk_number: u16,
    pub this_cd_entry_count: u16,
    pub total_cd_entry_count: u16,
    pub size_of_cd: u32,
    pub offset_of_cd_start: u32,
    pub file_comment_length: u16,
}

impl EndOfCentralDirectory {
    pub const SIGNATURE: u32 = 0x06054b50;
}
