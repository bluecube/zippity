#![allow(clippy::unreadable_literal)]

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
    #[packed_field(size_bytes = "4")]
    pub last_mod_datetime: DosDatetime,
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
    // TODO: The fields for language encoding and data descriptor should be swapped,
    // according to the APPNOTE. (3 and 11 instead of 11 and 3).
    // Hovewer for some reason this was serialized badly.
    // I'm pretty sure this is caused by my misuse of packed_struct, but this
    // works and  I'm done debugging this for now...
    // Might be related to https://github.com/hashmismatch/packed_struct.rs/issues/92
    #[packed_field(bits = "11")]
    pub use_data_descriptor: bool,

    /// Filename and comments in UTF-8
    #[packed_field(bits = "3")]
    pub language_encoding: bool,
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
    #[packed_field(size_bytes = "2")]
    pub flags: GpBitFlag,
    #[packed_field(size_bytes = "2", ty = "enum")]
    pub compression: Compression,
    #[packed_field(size_bytes = "4")]
    pub last_mod_datetime: DosDatetime,
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
    pub spec_version: u8,
    #[packed_field(size_bytes = "1", ty = "enum")]
    pub os: VersionMadeByOs,
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

#[derive(Copy, Clone, Debug, Default, PackedStruct, PartialEq, Eq)]
#[packed_struct(endian = "lsb", size_bytes = "4")]
pub struct DosDatetime {
    time: u16,
    date: u16,
}

impl DosDatetime {
    /// Constructs a new DosDateTime. Returns any value is out of range.
    pub(crate) fn new(
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
    ) -> Option<Self> {
        if !(1980..(1980 + 128)).contains(&year) {
            return None;
        }
        if !(1..=12).contains(&month) {
            return None;
        }
        if !(1..=31).contains(&day) {
            return None;
        }
        if hour > 23 {
            return None;
        }
        if minute > 59 {
            return None;
        }
        if second > 59 {
            return None;
        }

        Some(DosDatetime {
            time: (second / 2) as u16 | ((minute as u16) << 5) | ((hour as u16) << 11),
            date: day as u16 | ((month as u16) << 5) | (((year - 1980) << 9) as u16),
        })
    }

    #[cfg(test)]
    pub fn year(&self) -> i32 {
        ((self.date >> 9) + 1980) as i32
    }

    #[cfg(test)]
    pub fn month(&self) -> u32 {
        ((self.date >> 5) & 15) as u32
    }

    #[cfg(test)]
    pub fn day(&self) -> u32 {
        (self.date & 31) as u32
    }

    #[cfg(test)]
    pub fn hour(&self) -> u32 {
        (self.time >> 11) as u32
    }

    #[cfg(test)]
    pub fn minute(&self) -> u32 {
        ((self.time >> 5) & 63) as u32
    }

    #[cfg(test)]
    pub fn second(&self) -> u32 {
        ((self.time & 31) * 2) as u32
    }
}

pub trait PackedStructZippityExt {
    fn packed_size() -> u64;
    fn packed_size_usize() -> usize;
    fn packed_size_u16() -> u16;
}

impl<T: PackedStruct> PackedStructZippityExt for T {
    fn packed_size() -> u64 {
        Self::packed_size_usize() as u64
    }

    fn packed_size_usize() -> usize {
        Self::packed_bytes_size(None).unwrap_or_else(|_| unreachable!("This never fails"))
    }

    fn packed_size_u16() -> u16 {
        Self::packed_size_usize()
            .try_into()
            .expect("The struct is small")
    }
}

#[cfg(test)]
mod tests {
    use super::DosDatetime;
    use assert2::assert;
    use test_strategy::proptest;

    #[test]
    fn valid_dosdatetime_creation() {
        // Test a valid DosDatetime creation within valid ranges
        let dt = DosDatetime::new(1985, 12, 25, 14, 30, 28);
        assert!(dt.is_some());
    }

    #[test]
    fn invalid_year_lower_bound() {
        // Year is below 1980
        let dt = DosDatetime::new(1979, 5, 10, 10, 20, 20);
        assert!(dt.is_none());
    }

    #[test]
    fn invalid_year_upper_bound() {
        // Year is above 2107 (1980 + 127)
        let dt = DosDatetime::new(2108, 5, 10, 10, 20, 20);
        assert!(dt.is_none());
    }

    #[test]
    fn invalid_month() {
        // Month is out of range (valid range is 1-12)
        let dt = DosDatetime::new(2000, 13, 10, 10, 20, 20);
        assert!(dt.is_none());
    }

    #[test]
    fn invalid_day() {
        // Day is out of range (valid range is 1-31)
        let dt = DosDatetime::new(2000, 5, 32, 10, 20, 20);
        assert!(dt.is_none());
    }

    #[test]
    fn invalid_hour() {
        // Hour is out of range (valid range is 0-23)
        let dt = DosDatetime::new(2000, 5, 10, 24, 20, 20);
        assert!(dt.is_none());
    }

    #[test]
    fn invalid_minute() {
        // Minute is out of range (valid range is 0-59)
        let dt = DosDatetime::new(2000, 5, 10, 10, 60, 20);
        assert!(dt.is_none());
    }

    #[test]
    fn invalid_second() {
        // Second is out of range (valid range is 0-59)
        let dt = DosDatetime::new(2000, 5, 10, 10, 20, 60);
        assert!(dt.is_none());
    }

    #[test]
    fn even_second_rounding() {
        // Test that odd seconds round down to even
        let dt = DosDatetime::new(2000, 5, 10, 10, 20, 31);
        assert!(dt.is_some());
        let datetime = dt.unwrap();
        // Check that the seconds were rounded down to 30
        assert!(datetime.time & 0b11111 == 15); // 15 represents 30 seconds (rounded down)
    }

    #[proptest]
    fn dos_datetime_round_trip(
        #[strategy(1980..2108)] year: i32,
        #[strategy(1u32..=12u32)] month: u32,
        #[strategy(1u32..=31u32)] day: u32,
        #[strategy(0u32..24u32)] hour: u32,
        #[strategy(0u32..60u32)] minute: u32,
        #[strategy(0u32..60u32)] second: u32,
    ) {
        let dt = DosDatetime::new(year, month, day, hour, minute, second).unwrap();

        assert!(dt.year() == year);
        assert!(dt.month() == month);
        assert!(dt.day() == day);
        assert!(dt.hour() == hour);
        assert!(dt.minute() == minute);
        assert!(dt.second() == second & 0xfe);
    }
}
