use std::collections::BTreeMap;
use std::io::Result;

use crate::{
    entry_data::EntryData,
    error::ZippityError,
    reader::{Reader, ReaderEntry, Sizes},
    structs::{self, PackedStructZippityExt},
};

#[derive(Clone, Debug)]
pub struct BuilderEntry<D> {
    data: D,
    crc32: Option<u32>,
}

impl<D: EntryData> BuilderEntry<D> {
    pub fn crc32(&mut self, crc32: u32) -> &mut Self {
        self.crc32 = Some(crc32);
        self
    }
    fn get_local_size(&self, name: &str, data_size: u64) -> u64 {
        let local_header = structs::LocalFileHeader::packed_size();
        let zip64_extra_data = structs::Zip64ExtraField::packed_size();
        let filename = name.len() as u64;
        let data_descriptor = structs::DataDescriptor64::packed_size();

        local_header + zip64_extra_data + filename + data_size + data_descriptor
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
    // TODO: BTreeMap? Really? Perhaps we should use something that preserves insertion order.
    entries: BTreeMap<String, BuilderEntry<D>>,
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
            entries: BTreeMap::new(),
        }
    }

    /// Adds an entry to the zip file.
    /// The returned reference can be used to add metadata to the entry.
    /// # Errors
    /// Will return an error if `name` is longer than `u16::MAX` (limitation of Zip format)
    pub fn add_entry<T: Into<D>>(
        &mut self,
        name: String,
        data: T,
    ) -> std::result::Result<&mut BuilderEntry<D>, ZippityError> {
        use std::collections::btree_map::Entry::{Occupied, Vacant};

        if u16::try_from(name.len()).is_err() {
            return Err(ZippityError::TooLongEntryName { entry_name: name });
        }
        let map_vacant_entry = match self.entries.entry(name) {
            Vacant(e) => e,
            Occupied(e) => {
                return Err(ZippityError::DuplicateEntryName {
                    entry_name: e.key().clone(),
                });
            }
        };

        let data = data.into();
        let inserted = map_vacant_entry.insert(BuilderEntry { data, crc32: None });
        Ok(inserted)
    }

    pub async fn build(self) -> Result<Reader<D>> {
        let mut offset: u64 = 0;
        let mut cd_size: u64 = 0;
        let entries = {
            let mut entries = Vec::with_capacity(self.entries.len());
            for (name, entry) in self.entries.into_iter() {
                let data_size = entry.data.size().await?;
                let local_size = entry.get_local_size(&name, data_size);
                let offset_before_entry = offset;
                offset += local_size;
                cd_size += BuilderEntry::<D>::get_cd_header_size(&name);
                entries.push(ReaderEntry::new(
                    name,
                    entry.data,
                    offset_before_entry,
                    entry.crc32,
                    data_size,
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

        Ok(Reader::new(
            Sizes {
                cd_offset,
                cd_size,
                eocd_offset,
                total_size,
            },
            entries,
        ))
    }
}

#[cfg(test)]
mod test {
    use crate::proptest::TestEntryData;

    use super::*;
    use assert2::assert;
    use assert_matches::assert_matches;

    use std::collections::HashMap;

    use test_strategy::proptest;

    #[test]
    fn too_long_enty_name() {
        let mut builder: Builder<()> = Builder::new();

        let name_length = u16::MAX as usize + 1;
        let e = builder.add_entry("X".repeat(name_length), ()).unwrap_err();
        assert_matches!(e, ZippityError::TooLongEntryName { entry_name } if entry_name.len() == name_length);
    }

    /// Tests an internal property of the builder -- that the sizes generated
    /// during building actually match the chunk size.
    #[proptest(async = "tokio")]
    async fn local_size_matches_chunks(content: TestEntryData) {
        use crate::reader::Chunk;

        let mut builder: Builder<&[u8]> = Builder::new();

        content.0.iter().for_each(|(name, value)| {
            builder.add_entry(name.clone(), value.as_ref()).unwrap();
        });

        let local_sizes = {
            let mut local_sizes = HashMap::with_capacity(builder.entries.len());

            for (k, v) in builder.entries.iter() {
                let size = v.data.size().await.unwrap();
                local_sizes.insert(k.clone(), v.get_local_size(k, size));
            }

            local_sizes
        };

        let zippity = builder.build().await.unwrap();

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
}
