use packed_struct::prelude::*;

/// Local file header
/// Preceedes every file.
/// Must be followed by file name nad extra fields (length is part of this struct)
#[derive(Debug, PackedStruct)]
#[packed_struct(endian = "lsb")]
pub struct LocalFileHeader {
    pub signature: u32,
    pub version_to_extract: u16,
    #[packed_field(size_bytes = "2")]
    pub flags: GpBitFlag,
    #[packed_field(size_bytes = "2", ty = "enum")]
    pub compression: Compression,
    pub last_mod_time: u16,
    pub last_mod_date: u16,
    pub crc32: u32,
    pub compressed_size: u32,
    pub uncompressed_size: u32,
    pub file_name_len: u16,
    pub extra_field_len: u16,
}

impl LocalFileHeader {
    pub const SIGNATURE: u32 = 0x04034b50;
}

#[derive(Debug, PackedStruct)]
#[packed_struct(bit_numbering = "lsb0", size_bytes = "2")]
pub struct GpBitFlag {
    #[packed_field(bits = "3")]
    pub use_data_descriptor: bool,
}

#[derive(Debug, PackedStruct)]
#[packed_struct(endian = "lsb")]
pub struct Zip64ExtraField {
    pub tag: u16,
    pub size: u16,
    pub uncompressed_size: u64,
    pub compressed_size: u64,
    pub offset: u64,
}

impl Zip64ExtraField {
    pub const TAG: u16 = 0x0001;
}

/// Zip64 version of the data descriptor
/// Follows file data.
#[derive(Debug, PackedStruct)]
#[packed_struct(endian = "lsb")]
pub struct DataDescriptor64 {
    pub signature: u32,
    pub crc32: u32,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
}

impl DataDescriptor64 {
    pub const SIGNATURE: u32 = 0x08074b50;
}

/// Central directory header
/// On per each file, placed in central directory.
#[derive(Debug, PackedStruct)]
#[packed_struct(endian = "lsb")]
pub struct CentralDirectoryHeader {
    pub signature: u32,
    #[packed_field(size_bytes = "2")]
    pub version_made_by: VersionMadeBy,
    pub version_to_extract: u16,
    pub flags: u16,
    #[packed_field(size_bytes = "2", ty = "enum")]
    pub compression: Compression,
    pub last_mod_time: u16,
    pub last_mod_date: u16,
    pub crc32: u32,
    pub compressed_size: u32,
    pub uncompressed_size: u32,
    pub file_name_len: u16,
    pub extra_field_len: u16,
    pub file_comment_length: u16,
    pub disk_number_start: u16,
    pub internal_attributes: u16,
    pub external_attributes: u32,
    pub local_header_offset: u32,
}

impl CentralDirectoryHeader {
    pub const SIGNATURE: u32 = 0x02014b50;
}

#[derive(Debug, PackedStruct)]
#[packed_struct(endian = "lsb")]
pub struct VersionMadeBy {
    #[packed_field(size_bytes = "1", ty = "enum")]
    pub os: VersionMadeByOs,
    pub spec_version: u8,
}

#[derive(Clone, Copy, Debug, PrimitiveEnum_u8)]
#[non_exhaustive]
pub enum VersionMadeByOs {
    Unix = 3,
}

#[derive(Debug, PackedStruct)]
#[packed_struct(endian = "lsb")]
pub struct Zip64EndOfCentralDirectoryRecord {
    pub signature: u32,
    pub size_of_zip64_eocd: u64,
    #[packed_field(size_bytes = "2")]
    pub version_made_by: VersionMadeBy,
    pub version_to_extract: u16,
    pub this_disk_number: u32,
    pub start_of_cd_disk_number: u32,
    pub this_cd_entry_count: u64,
    pub total_cd_entry_count: u64,
    pub size_of_cd: u64,
    pub cd_offset: u64,
}

impl Zip64EndOfCentralDirectoryRecord {
    pub const SIGNATURE: u32 = 0x06064b50;
}

#[derive(Debug, PackedStruct)]
#[packed_struct(endian = "lsb")]
pub struct Zip64EndOfCentralDirectoryLocator {
    pub signature: u32,
    pub start_of_cd_disk_number: u32,
    pub zip64_eocd_offset: u64,
    pub number_of_disks: u32,
}

impl Zip64EndOfCentralDirectoryLocator {
    pub const SIGNATURE: u32 = 0x07064b50;
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
    pub cd_offset: u32,
    pub file_comment_length: u16,
}

impl EndOfCentralDirectory {
    pub const SIGNATURE: u32 = 0x06054b50;
}

#[derive(Clone, Copy, Debug, PrimitiveEnum_u16)]
#[non_exhaustive]
pub enum Compression {
    Store = 0,
}

pub trait PackedStructZippityExt {
    fn packed_size() -> u64;
}

impl<T: PackedStruct> PackedStructZippityExt for T {
    fn packed_size() -> u64 {
        Self::packed_bytes_size(None).unwrap() as u64
    }
}
