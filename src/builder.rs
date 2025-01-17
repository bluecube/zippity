use indexmap::IndexMap;

use crate::{
    entry_data::EntryData,
    reader::{Reader, ReaderEntry, Sizes},
    structs::{self, PackedStructZippityExt},
    Error,
};

#[derive(Clone, Debug)]
pub struct BuilderEntry<D> {
    data: D,
    crc32: Option<u32>,
    datetime: Option<structs::DosDatetime>,
    file_type: structs::unix_mode::FileType,
    permissions: Option<u32>,
    data_size: u64,
}

impl<D: EntryData> BuilderEntry<D> {
    /// Sets the CRC32 of this entry.
    ///
    /// This is helpful, because if the `[Reader]` seeks over the file content, but still needs
    /// to output the CRC, we can just output this value instead of opening and the entry and
    /// calculating it.
    /// Providing a wrong value will be detected in some cases, but generally it will lead to
    /// a damaged zip file.
    /// The CRC32 values for a file can be obtained from the method `[Reader::crc32s()]` after
    /// it was calculated in the `[Reader]`.
    pub fn crc32(&mut self, crc32: u32) -> &mut Self {
        self.crc32 = Some(crc32);
        self
    }

    /// Sets the last modification date and time of the entry.
    /// Returns None if the date is out of the representable range (1980-1-1 to 2107-12-31)
    /// Note that only even seconds can be stored and the value will get rounded down.
    pub fn datetime(
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

    /// Sets the last modification date and time of the entry.
    /// If the date is out of the representable range (1980-1-1 to 2107-12-31), this method
    /// ignores the error and continues a default value (1980-1-1T00:00:00).
    /// Note that only even seconds can be stored and the value will get rounded down.
    pub fn datetime_or_default(
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

    /// Sets the entry to be a directory.
    /// This is the default.
    pub fn file(&mut self) -> &mut Self {
        self.file_type = structs::unix_mode::FileType::File;
        self
    }

    /// Sets the entry to be a directory.
    /// This method only modifies the unix permissions, directory entries should contain no data
    /// and the entry names should end with slash and, neither of which is enforced or
    /// verified by zippity.
    pub fn directory(&mut self) -> &mut Self {
        self.file_type = structs::unix_mode::FileType::Directory;
        self
    }

    /// Sets the entry to be a symlink.
    /// Symlinks don't support setting permissions will ignore any value set.
    pub fn symlink(&mut self) -> &mut Self {
        self.file_type = structs::unix_mode::FileType::Symlink;
        self
    }

    /// Sets the low 9 bits of unix permissions (with mask 0o777) of the entry.
    /// If permissions are not set, default is 0o755 for directories and 0o644 for files.
    /// Symlinks don't support setting permissions will ignore any value set.
    pub fn unix_permissions(&mut self, permissions: u32) -> &mut Self {
        self.permissions = Some(permissions & 0o777);
        self
    }

    fn get_local_size(&self, name: &str) -> u64 {
        let local_header = structs::LocalFileHeader::packed_size();
        let zip64_extra_data = structs::Zip64ExtraField::packed_size();
        let filename = name.len() as u64;
        let data_descriptor = structs::DataDescriptor64::packed_size();

        local_header + zip64_extra_data + filename + self.data_size + data_descriptor
    }

    fn get_cd_header_size(name: &str) -> u64 {
        let filename = name.len() as u64;
        let cd_entry = structs::CentralDirectoryHeader::packed_size();
        let zip64_extra_data = structs::Zip64ExtraField::packed_size();

        cd_entry + zip64_extra_data + filename
    }
}

#[derive(Clone, Debug)]
pub struct Builder<D: EntryData> {
    entries: IndexMap<String, BuilderEntry<D>>,
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
        }
    }

    /// Adds an entry to the zip file.
    /// The returned reference can be used to add metadata to the entry.
    ///
    /// # Errors
    /// Will return an error if `name` is longer than `u16::MAX` (limitation of Zip format),
    /// or the given entry name is already present in the archive.
    pub fn add_entry<T: Into<D>>(
        &mut self,
        name: String,
        data: T,
    ) -> std::result::Result<&mut BuilderEntry<D>, Error> {
        use indexmap::map::Entry;
        if u16::try_from(name.len()).is_err() {
            return Err(Error::TooLongEntryName { entry_name: name });
        }
        let map_vacant_entry = match self.entries.entry(name) {
            Entry::Vacant(e) => e,
            Entry::Occupied(e) => {
                return Err(Error::DuplicateEntryName {
                    entry_name: e.key().clone(),
                });
            }
        };

        let data = data.into();
        let data_size = data.size();
        let inserted = map_vacant_entry.insert(BuilderEntry {
            data,
            crc32: None,
            datetime: None,
            file_type: structs::unix_mode::FileType::File,
            permissions: None,
            data_size,
        });
        Ok(inserted)
    }

    pub fn build(self) -> Reader<D> {
        let mut offset: u64 = 0;
        let mut cd_size: u64 = 0;
        let entries = {
            let mut entries = Vec::with_capacity(self.entries.len());
            for (name, entry) in self.entries.into_iter() {
                let local_size = entry.get_local_size(&name);
                let offset_before_entry = offset;
                offset += local_size;
                cd_size += BuilderEntry::<D>::get_cd_header_size(&name);
                let external_attributes =
                    structs::unix_mode::get_external_attributes(entry.file_type, entry.permissions);

                entries.push(ReaderEntry::new(
                    name,
                    entry.data,
                    offset_before_entry,
                    entry.crc32,
                    entry.datetime.unwrap_or_default(),
                    external_attributes,
                    entry.data_size,
                ));
            }
            entries
        };

        let cd_offset = offset;
        let eocd_size = structs::Zip64EndOfCentralDirectoryRecord::packed_size()
            + structs::Zip64EndOfCentralDirectoryLocator::packed_size()
            + structs::EndOfCentralDirectory::packed_size();
        let eocd_offset = cd_offset + cd_size;
        let total_size = cd_offset + cd_size + eocd_size;

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
}

#[cfg(test)]
mod test {
    use crate::proptest::TestEntryData;

    use super::*;
    use assert2::assert;
    use assert_matches::assert_matches;
    use bytes::Bytes;

    use std::collections::HashMap;

    use test_strategy::proptest;

    #[tokio::test]
    async fn too_long_enty_name() {
        let mut builder: Builder<()> = Builder::new();

        let name_length = u16::MAX as usize + 1;
        let e = builder.add_entry("X".repeat(name_length), ()).unwrap_err();
        assert_matches!(e, Error::TooLongEntryName { entry_name } if entry_name.len() == name_length);
    }

    /// Tests an internal property of the builder -- that the sizes generated
    /// during building actually match the chunk size.
    #[proptest(async = "tokio")]
    async fn local_size_matches_chunks(content: TestEntryData) {
        use crate::reader::Chunk;

        let builder: Builder<Bytes> = content.into();

        let local_sizes = {
            let mut local_sizes = HashMap::with_capacity(builder.entries.len());

            for (k, v) in builder.entries.iter() {
                local_sizes.insert(k.clone(), v.get_local_size(k));
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

    #[proptest(async = "tokio")]
    async fn add_entry_stores_size_correctly(d: Vec<u8>) {
        let mut builder = Builder::<&[u8]>::new();
        let be = builder.add_entry("x".into(), d.as_ref()).unwrap();
        assert!(be.data_size == d.len() as u64);
    }

    #[test]
    fn datetime_default_works() {
        let mut builder = Builder::<()>::new();
        builder
            .add_entry("X".into(), ())
            .unwrap()
            .datetime_or_default(3000, 1, 1, 1, 1, 1);

        assert!(builder.entries["X"].datetime.is_none());
    }

    #[test]
    fn unix_permissions_masking() {
        let mut builder = Builder::<()>::new();
        builder
            .add_entry("X".into(), ())
            .unwrap()
            .unix_permissions(0o123456);

        assert!(builder.entries["X"].permissions == Some(0o456));
    }
}
