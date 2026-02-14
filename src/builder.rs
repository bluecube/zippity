use std::{
    fmt::Debug,
    fs::{Metadata, Permissions},
    rc::Rc,
    time::SystemTime,
};

#[cfg(feature = "chrono")]
use chrono::{Datelike, NaiveDateTime, TimeZone, Timelike};
use indexmap::IndexMap;
use thiserror::Error;

use crate::{
    entry_data::EntryData,
    reader::{Reader, ReaderEntry, Sizes},
    structs::{self, PackedStructZippityExt},
};

type TimeConverter = Rc<dyn Fn(SystemTime) -> (i32, u32, u32, u32, u32, u32)>;

/// Represents a ZIP file entry in the metadata building phase.
/// Reference to this struct is returned when adding a new entry and allows modifying its metadata.
#[derive(Clone)]
pub struct BuilderEntry<D> {
    data: D,
    crc32: Option<u32>,
    datetime: Option<structs::DosDatetime>,
    file_type: BuilderFileType,
    permissions: BuilderPermissions,
    time_converter: Option<TimeConverter>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum BuilderPermissions {
    Rw,
    Ro,
    UnixPermissions(u32),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum BuilderFileType {
    File,
    Directory,
    Symlink,
}

impl<D: EntryData + Debug> Debug for BuilderEntry<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuilderEntry")
            .field("data", &self.data)
            .field("crc32", &self.crc32)
            .field("datetime", &self.datetime)
            .field("file_type", &self.file_type)
            .field("permissions", &self.permissions)
            .finish_non_exhaustive()
    }
}

impl<D: EntryData> BuilderEntry<D> {
    fn new(data: D, time_converter: Option<TimeConverter>) -> BuilderEntry<D> {
        BuilderEntry {
            data,
            crc32: None,
            datetime: None,
            file_type: BuilderFileType::File,
            permissions: BuilderPermissions::Rw,
            time_converter,
        }
    }

    /// Sets the CRC32 of this entry.
    ///
    /// This is helpful, because if the [`Reader`] seeks over the file content, but still needs
    /// to output the CRC, we can just output this value instead of opening and the entry and
    /// calculating it.
    /// Providing a wrong value will in some cases be detected during reading
    /// (as [`crate::ReadError::Crc32Mismatch`]), but generally it will lead to a damaged zip file.
    /// The CRC32 values for a file can be obtained from the method [`Reader::crc32s()`].
    pub fn crc32(&mut self, crc32: u32) -> &mut Self {
        self.crc32 = Some(crc32);
        self
    }

    /// Sets the last modification date and time of the entry using date and time fields.
    ///
    /// Returns `None` if the date is out of the representable range (1980-01-01 to 2107-12-31).
    ///
    /// Note that only even seconds can be stored; the value will be rounded down to the nearest even second.
    pub fn datetime_fields(
        &mut self,
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
    ) -> Option<&mut Self> {
        self.datetime = Some(structs::DosDatetime::new(
            year, month, day, hour, minute, second,
        )?);
        Some(self)
    }

    /// Sets the last modification date and time of the entry using date and time fields.
    ///
    /// If the date is out of the representable range (1980-01-01 to 2107-12-31), the datetime
    /// is set to the default value (1980-01-01T00:00:00).
    ///
    /// Note that only even seconds can be stored; the value will be rounded down to the nearest even second.
    ///
    /// This is equivalent to calling [`BuilderEntry::datetime_fields()`], but with a default fallback.
    pub fn datetime_fields_or_default(
        &mut self,
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
    ) -> &mut Self {
        self.datetime = structs::DosDatetime::new(year, month, day, hour, minute, second);
        self
    }

    /// Sets the last modification date and time of the entry using [`SystemTime`].
    ///
    /// Uses the time converter set by the last call to [`Builder::system_time_converter()`] or [`Builder::system_time_timezone()`].
    ///
    /// Returns `None` if the converter is not set, or the date is out of the representable range (1980-01-01 to 2107-12-31).
    ///
    /// Note that only even seconds can be stored; the value will be rounded down to the nearest even second.
    ///
    /// This is equivalent to calling [`BuilderEntry::datetime_fields()`] with converted [`SystemTime`].
    pub fn datetime_system(&mut self, datetime: SystemTime) -> Option<&mut Self> {
        let converter = self.time_converter.as_ref()?;
        let converted = converter(datetime);
        self.datetime_fields(
            converted.0,
            converted.1,
            converted.2,
            converted.3,
            converted.4,
            converted.5,
        )
    }

    /// Sets the last modification date and time of the entry using [`SystemTime`].
    ///
    /// Uses the time converter set by the last call to [`Builder::system_time_converter()`] or [`Builder::system_time_timezone()`].
    ///
    /// If the converter is not set, or the date is out of the representable range (1980-01-01 to 2107-12-31),
    /// the datetime is set to the default value (1980-01-01T00:00:00).
    ///
    /// Note that only even seconds can be stored; the value will be rounded down to the nearest even second.
    ///
    /// This is equivalent to calling [`BuilderEntry::datetime_fields_or_default()`] with converted [`SystemTime`].
    pub fn datetime_system_or_default(&mut self, datetime: SystemTime) -> &mut Self {
        self.datetime_system(datetime);
        self
    }

    #[cfg(feature = "chrono")]
    /// Sets the last modification date and time of the entry using [`NaiveDateTime`].
    ///
    /// Returns `None` if the date is out of the representable range (1980-01-01 to 2107-12-31).
    ///
    /// Note that only even seconds can be stored; the value will be rounded down to the nearest even second.
    ///
    /// This is equivalent to calling [`BuilderEntry::datetime_fields()`] with converted [`NaiveDateTime`].
    pub fn datetime(&mut self, datetime: NaiveDateTime) -> Option<&mut Self> {
        self.datetime_fields(
            datetime.year(),
            datetime.month(),
            datetime.day(),
            datetime.hour(),
            datetime.minute(),
            datetime.second(),
        )
    }

    #[cfg(feature = "chrono")]
    /// Sets the last modification date and time of the entry using [`NaiveDateTime`].
    ///
    /// If the date is out of the representable range (1980-01-01 to 2107-12-31), the datetime
    /// is set to the default value (1980-01-01T00:00:00).
    ///
    /// Note that only even seconds can be stored; the value will be rounded down to the nearest even second.
    ///
    /// This is equivalent to calling [`BuilderEntry::datetime_fields_or_default()`] with converted [`NaiveDateTime`].
    pub fn datetime_or_default(&mut self, datetime: NaiveDateTime) -> &mut Self {
        self.datetime_fields(
            datetime.year(),
            datetime.month(),
            datetime.day(),
            datetime.hour(),
            datetime.minute(),
            datetime.second(),
        );
        self
    }

    /// Sets the entry to be a file.
    ///
    /// This is the default.
    pub fn file(&mut self) -> &mut Self {
        self.file_type = BuilderFileType::File;
        self
    }

    /// Sets the entry to be a directory.
    ///
    /// This method only modifies the unix permissions, directory entries should contain no data
    /// and the entry names should end with slash and, neither of which is enforced or
    /// verified by zippity.
    pub fn directory(&mut self) -> &mut Self {
        self.file_type = BuilderFileType::Directory;
        self
    }

    /// Sets the entry to be a symlink.
    ///
    /// Symlinks don't support setting permissions and will ignore any value set.
    pub fn symlink(&mut self) -> &mut Self {
        self.file_type = BuilderFileType::Symlink;
        self
    }

    /// Sets the low 9 bits of unix permissions (with mask `0o777`) of the entry.
    ///
    /// If permissions are not set, default is `0o755` for directories and `0o644` for files.
    ///
    /// Symlinks don't support setting permissions and will ignore any value set.
    pub fn unix_permissions(&mut self, permissions: u32) -> &mut Self {
        self.permissions = BuilderPermissions::UnixPermissions(permissions & 0o777);
        self
    }

    /// Sets the entry permissions based on [`std::fs::Permissions`].
    /// On unix this is equivalent to calling [`BuilderEntry::unix_permissions()`],
    /// on non-unix systems this sets the file as RO or RW.
    pub fn permissions(&mut self, permissions: &Permissions) -> &mut Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            self.unix_permissions(permissions.mode());
        };
        #[cfg(not(unix))]
        {
            self.ro(permissions.readonly());
        }
        self
    }

    /// Sets the entry permissions to be read only or read-write.
    /// This overrides anything set using [`BuilderEntry::permissions()`] or [`BuilderEntry::unix_permissions()`].
    pub fn readonly(&mut self, ro: bool) -> &mut Self {
        self.permissions = if ro {
            BuilderPermissions::Ro
        } else {
            BuilderPermissions::Rw
        };
        self
    }

    /// Sets entry type (directory / symlink / file), unix permissions and modification time from [`std::fs::Metadata`].
    ///
    /// Uses the time converter set by the last call to [`Builder::system_time_converter()`]
    /// or [`Builder::system_time_timezone()`]. If the converter is not set, or the date is
    /// out of the representable range (1980-01-01 to 2107-12-31), the modification time
    /// is set to the default value (1980-01-01T00:00:00).
    ///
    /// Note that only even seconds can be stored; the value will be rounded down to the nearest even second.
    pub fn metadata(&mut self, metadata: &Metadata) -> &mut Self {
        if metadata.is_dir() {
            self.directory();
        } else if metadata.is_symlink() {
            self.symlink();
        } else {
            self.file();
        }
        self.permissions(&metadata.permissions());
        if let Ok(modified) = metadata.modified() {
            // We're skiping the modification time on platforms where the file metadata don't contain it
            self.datetime_system_or_default(modified);
        }

        self
    }
}

fn local_header_size(name: &str) -> u64 {
    let local_header = structs::LocalFileHeader::packed_size();
    let zip64_extra_data = structs::Zip64ExtraField::packed_size();
    let filename = name.len() as u64;
    let data_descriptor = structs::DataDescriptor64::packed_size();

    local_header + zip64_extra_data + filename + data_descriptor
}

fn cd_header_size(name: &str) -> u64 {
    let filename = name.len() as u64;
    let cd_entry = structs::CentralDirectoryHeader::packed_size();
    let zip64_extra_data = structs::Zip64ExtraField::packed_size();

    cd_entry + zip64_extra_data + filename
}

pub(crate) fn eocd_size() -> u64 {
    structs::Zip64EndOfCentralDirectoryRecord::packed_size()
        + structs::Zip64EndOfCentralDirectoryLocator::packed_size()
        + structs::EndOfCentralDirectory::packed_size()
}

/// Represents entries of the zip file, which can be converted to a [`Reader`].
#[derive(Clone)]
pub struct Builder<D: EntryData> {
    entries: IndexMap<String, BuilderEntry<D>>,
    time_converter: Option<TimeConverter>,
    /// Total size of the zip.
    total_size: u64,
}

/// An error that can be encountered while adding entries to a [`Builder`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AddEntryError {
    /// Entry name is longer than `u16::MAX`
    #[error("Entry name too long")] // Not mentioning the name here, because it's too long :)
    TooLongEntryName {
        /// The name of the entry that was too long.
        entry_name: String,
    },
    /// Adding entry would cause the ZIP file to be longer than `u64::MAX`
    #[error("Adding entry {entry_name} would cause the ZIP file to be longer than u64::MAX")]
    TooLongZipFile {
        /// The name of the entry that would make the zip file too long.
        entry_name: String,
    },
    /// Duplicate entry name
    #[error("Duplicate entry name {entry_name}")]
    DuplicateEntryName {
        /// The name of the entry that was duplicated.
        entry_name: String,
    },
}

impl<D: EntryData + Debug> Debug for Builder<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Builder")
            .field("entries", &self.entries)
            .field(
                "time_converter",
                match self.time_converter {
                    Some(_) => &"Some(_)",
                    None => &"None",
                },
            )
            .field("total_size", &self.total_size)
            .finish()
    }
}

impl<D: EntryData> Default for Builder<D> {
    fn default() -> Self {
        Builder::new()
    }
}

impl<D: EntryData> Builder<D> {
    /// Creates a new empty Builder.
    pub fn new() -> Self {
        Builder {
            entries: IndexMap::new(),
            time_converter: None,
            total_size: eocd_size(),
        }
    }

    /// Adds an entry to the zip file.
    /// The returned reference can be used to modify metadata to the entry.
    ///
    /// # Errors
    /// Returns an error if `name` is longer than `u16::MAX` (limitation of Zip format),
    /// or the given entry name is already present in the archive.
    /// Returns an error if the size of the zip file would become larger than `u64::MAX` after the add.
    pub fn add_entry<T: Into<D>>(
        &mut self,
        name: String,
        data: T,
    ) -> std::result::Result<&mut BuilderEntry<D>, AddEntryError> {
        use indexmap::map::Entry;

        if u16::try_from(name.len()).is_err() {
            return Err(AddEntryError::TooLongEntryName { entry_name: name });
        }
        let headers_size = local_header_size(&name) + cd_header_size(&name);

        let map_vacant_entry = match self.entries.entry(name) {
            Entry::Vacant(e) => e,
            Entry::Occupied(e) => {
                return Err(AddEntryError::DuplicateEntryName {
                    entry_name: e.key().clone(),
                });
            }
        };

        let data = data.into();
        self.total_size = match headers_size
            .checked_add(data.size())
            .and_then(|s| s.checked_add(self.total_size))
        {
            Some(size) => size,
            None => {
                return Err(AddEntryError::TooLongZipFile {
                    entry_name: map_vacant_entry.into_key(),
                });
            }
        };

        let inserted =
            map_vacant_entry.insert(BuilderEntry::new(data, self.time_converter.clone()));
        Ok(inserted)
    }

    /// Returns the size of the zip file if it was built immediately.
    ///
    /// Returns the same value as `self.build().size()`.
    pub fn size(&self) -> u64 {
        self.total_size
    }

    /// Finishes the build phase of the ZIP file and converts the builder into a [`Reader`].
    #[must_use]
    pub fn build(self) -> Reader<D> {
        // All size calculations in this method add up to self.total_size, so they are guaranteed to not overflow.
        let mut offset: u64 = 0;
        let mut cd_size: u64 = 0;
        let entries = {
            let mut entries = Vec::with_capacity(self.entries.len());
            for (name, entry) in self.entries {
                let entry_data_size = entry.data.size();
                let offset_before_entry = offset;
                offset += local_header_size(&name) + entry_data_size;
                cd_size += cd_header_size(&name);
                let external_attributes =
                    get_external_attributes(entry.file_type, entry.permissions);

                entries.push(ReaderEntry::new(
                    name,
                    entry.data,
                    offset_before_entry,
                    entry.crc32,
                    entry.datetime.unwrap_or_default(),
                    external_attributes,
                    entry_data_size,
                ));
            }
            entries
        };

        let cd_offset = offset;
        let eocd_offset = cd_offset + cd_size;
        let total_size = eocd_offset + eocd_size();

        Reader::new(
            Sizes {
                cd_offset,
                cd_size,
                eocd_offset,
                total_size,
            },
            entries,
        )
    }

    /// Sets a converter function that will be used for converting system times to field representation.
    ///
    /// The converter is passed to newly created builder entries and used in [`BuilderEntry::datetime_system()`],
    /// [`BuilderEntry::datetime_system_or_default()`] and [`BuilderEntry::metadata()`].
    pub fn system_time_converter<F>(&mut self, converter: F) -> &mut Self
    where
        F: Fn(SystemTime) -> (i32, u32, u32, u32, u32, u32) + 'static,
    {
        self.time_converter = Some(Rc::new(converter));
        self
    }

    /// Wrapper over [`Builder::system_time_converter()`] which sets a converter function based on a [`chrono::TimeZone`].
    ///
    /// The converter is passed to newly created builder entries and used in [`BuilderEntry::datetime_system()`],
    /// [`BuilderEntry::datetime_system_or_default()`] and [`BuilderEntry::metadata()`].
    #[cfg(feature = "chrono")]
    pub fn system_time_timezone<Tz>(&mut self, tz: Tz) -> &mut Self
    where
        Tz: TimeZone + 'static,
    {
        self.time_converter = Some(Rc::new(system_timezone_converter(tz)));
        self
    }

    /// Returns a reference to all entries currently in the builder.
    pub fn get_entries(&self) -> &IndexMap<String, BuilderEntry<D>> {
        &self.entries
    }

    /// Returns a mutable reference to all entries currently in the builder.
    pub fn get_entries_mut(&mut self) -> &mut IndexMap<String, BuilderEntry<D>> {
        &mut self.entries
    }
}

/// Creates a converter function suitable for [`Builder::system_time_converter()`], that
/// uses chrono and the given timezone to build the field representation.
///
/// This is the internal behavior of [`Builder::system_time_timezone()`], refactored out
/// for access during testing.
#[cfg(feature = "chrono")]
fn system_timezone_converter<Tz>(
    tz: Tz,
) -> impl Fn(SystemTime) -> (i32, u32, u32, u32, u32, u32) + 'static
where
    Tz: TimeZone + 'static,
{
    use chrono::{DateTime, Utc};

    move |system_time| {
        let naive = DateTime::<Utc>::from(system_time)
            .with_timezone(&tz)
            .naive_local();
        (
            naive.year(),
            naive.month(),
            naive.day(),
            naive.hour(),
            naive.minute(),
            naive.second(),
        )
    }
}

/// Converts filetype and permissions to a value usable as external attributes of zip entry.
/// The file type controls both the top bits of the unix mode and the default permissions.
/// Symlinks have permissions always set to 0o777 and ignore the parameter.
fn get_external_attributes(file_type: BuilderFileType, permissions: BuilderPermissions) -> u32 {
    let file_type_bits = match file_type {
        BuilderFileType::File => 0o100_000,
        BuilderFileType::Directory => 0o040_000,
        BuilderFileType::Symlink => 0o120_000,
    };

    let perm_bits = if file_type == BuilderFileType::Symlink {
        0o777
    } else {
        let executable_bits = match file_type {
            BuilderFileType::Directory => 0o111,
            _ => 0,
        };

        match permissions {
            BuilderPermissions::Rw => 0o644 | executable_bits,
            BuilderPermissions::Ro => 0o444 | executable_bits,
            BuilderPermissions::UnixPermissions(perms) => perms & 0o777,
        }
    };

    (file_type_bits | perm_bits) << 16
}

#[cfg(test)]
mod test {
    use crate::test_util::test_entry_data::TestEntryData;

    use super::*;
    use assert_matches::assert_matches;
    use assert2::assert;
    use structs::DosDatetime;

    use std::{collections::HashMap, time::UNIX_EPOCH};

    #[cfg(feature = "chrono")]
    use proptest::prop_assume;

    #[cfg(feature = "chrono")]
    use chrono::FixedOffset;

    use test_case::test_matrix;
    use test_strategy::proptest;

    #[tokio::test]
    async fn too_long_enty_name() {
        let mut builder: Builder<&[u8]> = Builder::new();

        let name_length = u16::MAX as usize + 1;
        let e = builder
            .add_entry("X".repeat(name_length), b"".as_ref())
            .unwrap_err();
        assert_matches!(e, AddEntryError::TooLongEntryName { entry_name } if entry_name.len() == name_length);
    }

    mod too_large_zip {
        use super::*;
        use crate::test_util::funky_entry_data::Zeros;

        #[tokio::test]
        async fn single_entry_does_not_fit() {
            let mut builder: Builder<Zeros> = Builder::new();

            let e = builder
                .add_entry("x".to_string(), Zeros::new(u64::MAX))
                .unwrap_err();
            assert_matches!(e, AddEntryError::TooLongZipFile { entry_name } if entry_name == "x");
        }

        #[tokio::test]
        async fn two_entries_header_fits_data_does_not() {
            let mut builder: Builder<Zeros> = Builder::new();

            let entry = Zeros::new(u64::MAX - 1024);
            let _ = builder.add_entry("x".to_string(), entry.clone()).unwrap();
            let e = builder.add_entry("y".to_string(), entry).unwrap_err();
            assert_matches!(e, AddEntryError::TooLongZipFile { entry_name } if entry_name == "y");
        }

        #[tokio::test]
        async fn two_entries_header_does_not_fit() {
            let mut builder: Builder<Zeros> = Builder::new();

            // Entry is sized so that it perfectly fits inside the limit
            dbg!(local_header_size("x") + cd_header_size("y") + eocd_size());
            let entry =
                Zeros::new(u64::MAX - local_header_size("x") - cd_header_size("y") - eocd_size());
            let _ = builder.add_entry("x".to_string(), entry.clone()).unwrap();
            let e = builder.add_entry("y".to_string(), entry).unwrap_err();
            assert_matches!(e, AddEntryError::TooLongZipFile { entry_name } if entry_name == "y");
        }
    }

    /// Tests an internal property of the builder -- that the sizes generated
    /// during building actually match the chunk size.
    #[proptest(async = "tokio")]
    async fn local_size_matches_chunks(content: TestEntryData) {
        use crate::reader::Chunk;

        let builder = Builder::from(content);

        let local_sizes = {
            let mut local_sizes = HashMap::with_capacity(builder.entries.len());

            for (k, v) in &builder.entries {
                local_sizes.insert(k.clone(), local_header_size(k) + v.data.size());
            }

            local_sizes
        };

        let zippity = builder.build();

        let entries = zippity.get_entries();

        for i in 0..entries.len() {
            let mut entry_local_size = 0;
            let mut chunk = Chunk::LocalHeader { entry_index: i };
            loop {
                dbg!(&chunk);
                dbg!(chunk.size(entries));
                entry_local_size += chunk.size(entries);
                chunk = chunk.next(entries);

                if let Chunk::LocalHeader { entry_index: _ } = chunk {
                    break;
                }
                if let Chunk::CDFileHeader { entry_index: _ } = chunk {
                    break;
                }
            }

            assert!(local_sizes[entries[i].get_name()] == entry_local_size);
        }
    }

    #[test]
    fn empty_builder_size_matches_reader_size() {
        let builder = Builder::<&[u8]>::new();
        let size = builder.size();
        let reader = builder.build();

        assert!(size == reader.size());
    }

    #[proptest]
    fn builder_size_matches_reader_size(content: TestEntryData) {
        let builder = Builder::from(content);
        let size = builder.size();
        let reader = builder.build();

        assert!(size == reader.size());
    }

    fn datetime_fields(entry: &mut BuilderEntry<&[u8]>, year: i32, is_some: bool) {
        assert!(entry.datetime_fields(year, 1, 1, 1, 1, 1).is_some() == is_some);
    }

    fn datetime_fields_or_default(entry: &mut BuilderEntry<&[u8]>, year: i32, _is_some: bool) {
        entry.datetime_fields_or_default(year, 1, 1, 1, 1, 1);
    }

    #[cfg(feature = "chrono")]
    fn datetime(entry: &mut BuilderEntry<&[u8]>, year: i32, is_some: bool) {
        let chrono_datetime = chrono::NaiveDate::from_ymd_opt(year, 1, 1)
            .unwrap()
            .and_hms_opt(1, 1, 1)
            .unwrap();
        assert!(entry.datetime(chrono_datetime).is_some() == is_some);
    }

    #[cfg(feature = "chrono")]
    fn datetime_or_default(entry: &mut BuilderEntry<&[u8]>, year: i32, _is_some: bool) {
        let chrono_datetime = chrono::NaiveDate::from_ymd_opt(year, 1, 1)
            .unwrap()
            .and_hms_opt(1, 1, 1)
            .unwrap();
        entry.datetime_or_default(chrono_datetime);
    }

    fn datetime_system(entry: &mut BuilderEntry<&[u8]>, year: i32, is_some: bool) {
        entry.time_converter = Some(Rc::new(move |_| (year, 1, 1, 1, 1, 1)));
        assert!(entry.datetime_system(UNIX_EPOCH).is_some() == is_some);
    }

    fn datetime_system_or_default(entry: &mut BuilderEntry<&[u8]>, year: i32, _is_some: bool) {
        entry.time_converter = Some(Rc::new(move |_| (year, 1, 1, 1, 1, 1)));
        entry.datetime_system_or_default(UNIX_EPOCH);
    }

    #[cfg_attr(feature = "chrono", test_matrix(
        [
            datetime_fields,
            datetime_fields_or_default,
            datetime,
            datetime_or_default,
            datetime_system,
            datetime_system_or_default,
        ],
        [2024, 1000]
    ))]
    #[cfg_attr(not(feature = "chrono"), test_matrix(
        [
            datetime_fields,
            datetime_fields_or_default,
            datetime_system,
            datetime_system_or_default,
        ],
        [2024, 1000]
    ))]
    fn datetime_test(setter_fn: impl FnOnce(&mut BuilderEntry<&[u8]>, i32, bool), year: i32) {
        let is_some = year >= 1980;

        let mut entry = BuilderEntry::new(b"".as_ref(), None);

        setter_fn(&mut entry, year, is_some);

        assert!(entry.datetime.is_some() == is_some);
        if let Some(dos_datetime) = entry.datetime {
            assert!(dos_datetime == DosDatetime::new(year, 1, 1, 1, 1, 1).unwrap());
        }
    }

    #[cfg(feature = "chrono")]
    #[proptest]
    fn datetime_chrono_matches(
        #[strategy(1900..2100)] year: i32,
        #[strategy(1u32..=12u32)] month: u32,
        #[strategy(1u32..=31u32)] day: u32,
        #[strategy(0u32..24u32)] hour: u32,
        #[strategy(0u32..60u32)] minute: u32,
        #[strategy(0u32..60u32)] second: u32,
    ) {
        let Some(chrono_datetime) = chrono::NaiveDate::from_ymd_opt(year, month, day)
            .and_then(|d| d.and_hms_opt(hour, minute, second))
        else {
            prop_assume!(false);
            unreachable!();
        };

        let mut entry1 = BuilderEntry::new(b"".as_ref(), None);
        let mut entry2 = entry1.clone();

        entry1.datetime_fields(year, month, day, hour, minute, second);
        entry2.datetime(chrono_datetime);

        assert!(entry1.datetime == entry2.datetime);
    }

    #[test]
    fn system_time_conversion_comes_from_builder() {
        let mut builder: Builder<&[u8]> = Builder::new();
        builder.system_time_converter(|_| (2000, 1, 2, 3, 4, 5));

        let x: &BuilderEntry<&[u8]> = builder
            .add_entry("X".into(), b"".as_ref())
            .unwrap()
            .datetime_system(SystemTime::UNIX_EPOCH)
            .unwrap();
        let mut y = BuilderEntry::new(b"", None);
        y.datetime_fields(2000, 1, 2, 3, 4, 5).unwrap();

        assert!(x.datetime == y.datetime);
    }

    #[cfg(feature = "chrono")]
    #[proptest]
    fn system_time_timezeone(#[strategy(-5..=5)] offset: i32) {
        let timezone = FixedOffset::east_opt(offset).unwrap();

        let mut builder: Builder<&[u8]> = Builder::new();

        builder.system_time_timezone(timezone);

        let chrono_datetime = chrono::NaiveDate::from_ymd_opt(2025, 2, 2)
            .unwrap()
            .and_hms_opt(17, 0, 0)
            .unwrap()
            .and_utc();
        let system_datetime: SystemTime = chrono_datetime.into();
        let chrono_converted = chrono_datetime.with_timezone(&timezone).naive_local();

        let converter = builder.time_converter.unwrap();
        let converted = converter(system_datetime);

        assert!(converted.0 == chrono_converted.year());
        assert!(converted.1 == chrono_converted.month());
        assert!(converted.2 == chrono_converted.day());
        assert!(converted.3 == chrono_converted.hour());
        assert!(converted.4 == chrono_converted.minute());
        assert!(converted.5 == chrono_converted.second());
    }

    #[test]
    fn unix_permissions_masking() {
        let mut entry = BuilderEntry::new(b"".as_ref(), None);
        entry.unix_permissions(0o123_456);

        assert!(entry.permissions == BuilderPermissions::UnixPermissions(0o456));
    }

    mod metadata {
        use std::{fs, os::unix::fs::PermissionsExt};

        use assert2::assert;
        use tempfile::TempDir;

        use super::*;

        #[test]
        fn readonly() {
            let mut entry = BuilderEntry::new(b"".as_ref(), None);
            assert!(entry.permissions == BuilderPermissions::Rw);
            entry.readonly(true);
            assert!(entry.permissions == BuilderPermissions::Ro);
            entry.readonly(false);
            assert!(entry.permissions == BuilderPermissions::Rw);
        }

        #[proptest]
        fn unix_permissions(#[strategy(0u32..0o777u32)] permissions: u32) {
            let mut entry = BuilderEntry::new(b"".as_ref(), None);
            entry.unix_permissions(permissions);
            assert!(entry.permissions == BuilderPermissions::UnixPermissions(permissions));
        }

        #[test]
        fn file() {
            let tempdir = TempDir::new().unwrap();

            let path = tempdir.as_ref().join("asdf");
            fs::write(&path, b"hello world").unwrap();
            let metadata = fs::symlink_metadata(path).unwrap();

            let mut entry = BuilderEntry::new(b"".as_ref(), None);
            entry.metadata(&metadata);
            assert!(entry.file_type == BuilderFileType::File);
        }

        #[test]
        fn directory() {
            let tempdir = TempDir::new().unwrap();

            let path = tempdir.as_ref().join("asdf");
            fs::create_dir(&path).unwrap();
            let metadata = fs::symlink_metadata(path).unwrap();

            let mut entry = BuilderEntry::new(b"".as_ref(), None);
            entry.metadata(&metadata);
            assert!(entry.file_type == BuilderFileType::Directory);
        }

        #[test]
        fn symlink() {
            let tempdir = TempDir::new().unwrap();

            let path1 = tempdir.as_ref().join("asdf");
            let path2 = tempdir.as_ref().join("efgh");
            fs::write(&path1, b"hello world").unwrap();
            std::os::unix::fs::symlink(&path1, &path2).unwrap();
            dbg!(&path2);
            let metadata = fs::symlink_metadata(path2).unwrap();

            let mut entry = BuilderEntry::new(b"".as_ref(), None);
            entry.metadata(&metadata);
            assert!(entry.file_type == BuilderFileType::Symlink);
        }

        #[cfg(unix)]
        #[proptest]
        fn file_permissions(#[strategy(0u32..0o777u32)] permissions: u32) {
            let tempdir = TempDir::new().unwrap();

            let path = tempdir.as_ref().join("asdf");
            fs::write(&path, b"hello world").unwrap();
            let metadata = fs::symlink_metadata(&path).unwrap();
            let mut p = metadata.permissions();
            p.set_mode(permissions);
            fs::set_permissions(&path, p).unwrap();
            let metadata = fs::symlink_metadata(path).unwrap();

            let mut entry = BuilderEntry::new(b"".as_ref(), None);
            entry.permissions(&metadata.permissions());
            assert!(entry.permissions == BuilderPermissions::UnixPermissions(permissions));
        }

        #[cfg(windows)]
        #[proptest]
        fn file_permissions_windows(readonly: bool) {
            let path = tempdir.as_ref().join("asdf");
            fs::write(&path, b"hello world").unwrap();
            let metadata = fs::symlink_metadata(&path).unwrap();
            let mut p = metadata.permissions();
            p.set_readonly(readonly);
            fs::set_permissions(&path, p).unwrap();
            let metadata = fs::symlink_metadata(path).unwrap();

            let mut entry = BuilderEntry::new(b"".as_ref(), None);
            entry.permissions(&metadata.permissions());

            let expected = if readonly { 0o444 } else { 0o666 };
            assert_eq!(entry.permissions, Some(expected));
        }

        /// Tests that manually setting modification time from metadata system
        /// time results in the same modification time as using `BuilderEntry::metadata`
        #[cfg(feature = "chrono")]
        #[test]
        fn modification_time1() {
            use chrono::Utc;

            let tempdir = TempDir::new().unwrap();

            let path = tempdir.as_ref().join("asdf");
            fs::write(&path, b"hello world").unwrap();
            let metadata = fs::symlink_metadata(path).unwrap();

            let mut entry1 =
                BuilderEntry::new(b"".as_ref(), Some(Rc::new(system_timezone_converter(Utc))));
            let mut entry2 = entry1.clone();

            entry1.metadata(&metadata);
            entry2.datetime_system(metadata.modified().unwrap());
            assert!(entry1.datetime == entry2.datetime);
        }

        /// Checks that stored modification time is within few seconds from `SystemTime::now()`
        /// obtained near the file creation.
        /// This test is potentially fragile.
        #[cfg(feature = "chrono")]
        #[test]
        fn modification_time2() {
            use chrono::{DateTime, NaiveDate, TimeDelta, Utc};

            let tempdir = TempDir::new().unwrap();

            let path = tempdir.as_ref().join("asdf");
            fs::write(&path, b"hello world").unwrap();
            let creation_time = SystemTime::now();
            let metadata = fs::symlink_metadata(path).unwrap();

            let mut entry =
                BuilderEntry::new(b"".as_ref(), Some(Rc::new(system_timezone_converter(Utc))));
            entry.metadata(&metadata);

            let entry_datetime = entry.datetime.expect("Entry must have the datetime set");
            let entry_datetime = NaiveDate::from_ymd_opt(
                entry_datetime.year(),
                entry_datetime.month(),
                entry_datetime.day(),
            )
            .unwrap()
            .and_hms_opt(
                entry_datetime.hour(),
                entry_datetime.minute(),
                entry_datetime.second(),
            )
            .unwrap()
            .and_utc();
            let creation_time = DateTime::<Utc>::from(creation_time);

            let difference = (entry_datetime - creation_time).abs();

            assert!(difference < TimeDelta::seconds(5));
        }
    }
}
