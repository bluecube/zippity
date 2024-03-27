use std::collections::BTreeMap;
use std::future::Future;
use std::io::{Error, Result, SeekFrom};
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use assert2::assert;
use packed_struct::{PackedStruct, PackedStructSlice};
use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncSeek, ReadBuf};

use crate::crc_reader::CrcReader;
use crate::entry_data::EntryData;
use crate::error::ZippityError;
use crate::structs;
use crate::structs::PackedStructZippityExt;

/// Minimum version needed to extract the zip64 extensions required by zippity
pub const ZIP64_VERSION_TO_EXTRACT: u8 = 45;

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
    fn get_local_size(&self, name: &str) -> u64 {
        let local_header = structs::LocalFileHeader::packed_size();
        let zip64_extra_data = structs::Zip64ExtraField::packed_size();
        let filename = name.len() as u64;
        let data = self.data.size();
        let data_descriptor = structs::DataDescriptor64::packed_size();

        local_header + zip64_extra_data + filename + data + data_descriptor
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

    pub fn build(self) -> Reader<D> {
        let mut offset: u64 = 0;
        let mut cd_size: u64 = 0;
        let entries: Vec<_> = self
            .entries
            .into_iter()
            .map(|(name, entry)| {
                let size = entry.get_local_size(&name);
                let offset_copy = offset;
                offset += size;
                cd_size += BuilderEntry::<D>::get_cd_header_size(&name);
                ReaderEntry {
                    name,
                    data: entry.data,
                    offset: offset_copy,
                    crc32: entry.crc32,
                }
            })
            .collect();

        let cd_offset = offset;
        let eocd_size = structs::Zip64EndOfCentralDirectoryRecord::packed_size()
            + structs::Zip64EndOfCentralDirectoryLocator::packed_size()
            + structs::EndOfCentralDirectory::packed_size();
        let eocd_offset = cd_offset + cd_size;
        let total_size = cd_offset + cd_size + eocd_size;
        let current_chunk = Chunk::new(&entries);

        Reader {
            sizes: Sizes {
                cd_offset,
                cd_size,
                eocd_offset,
                total_size,
            },
            entries,
            read_state: ReadState {
                current_chunk,
                chunk_processed_size: 0,
                position: 0,
                staging_buffer: Vec::new(),
                to_skip: 0,
            },
            pinned: ReaderPinned::Nothing,
        }
    }
}

#[derive(Debug)]
struct ReaderEntry<D: EntryData> {
    name: String,
    data: D,
    offset: u64,
    crc32: Option<u32>,
}

impl<D: EntryData> ReaderEntry<D> {
    fn get_reader<'a>(
        &self,
        mut pinned: Pin<&'a mut ReaderPinned<D>>,
        ctx: &mut Context<'_>,
    ) -> Poll<Result<Pin<&'a mut CrcReader<D::Reader>>>> {
        if let ReaderPinnedProj::Nothing = pinned.as_mut().project() {
            let reader_future = self.data.get_reader();
            pinned.set(ReaderPinned::ReaderFuture(reader_future));
        }

        if let ReaderPinnedProj::ReaderFuture(ref mut reader_future) = pinned.as_mut().project() {
            let reader = ready!(reader_future.as_mut().poll(ctx))?;
            pinned.set(ReaderPinned::FileReader(CrcReader::new(reader)));
        }

        match pinned.project() {
            ReaderPinnedProj::FileReader(reader) => Poll::Ready(Ok(reader)),
            _ => unreachable!(),
        }
    }

    /// Get the CRC for this entry, possibly recomputing it from the file.
    /// Stores state in `self.crc32` and `pinned`,
    /// Calling this method repeatedly will either return the already computed CRC without changes.
    /// or progress towards computing the CRC.
    fn get_crc(
        &mut self,
        mut pinned: Pin<&mut ReaderPinned<D>>,
        ctx: &mut Context<'_>,
        read_buffer: &mut Vec<u8>,
    ) -> Poll<Result<u32>> {
        if let Some(crc) = self.crc32 {
            return Poll::Ready(Ok(crc));
        }

        let mut file_reader = ready!(self.get_reader(pinned.as_mut(), ctx))?;

        assert!(read_buffer.is_empty());
        read_buffer.resize(8192, 0);

        loop {
            let mut read_buffer_wrapped = ReadBuf::new(read_buffer.as_mut_slice());
            let remaining_before = read_buffer_wrapped.remaining();
            ready!(file_reader
                .as_mut()
                .poll_read(ctx, &mut read_buffer_wrapped))?;
            if read_buffer_wrapped.remaining() == remaining_before {
                break;
            }
        }

        let crc32 = file_reader.get_crc32();

        pinned.set(ReaderPinned::Nothing);
        read_buffer.clear();

        self.crc32 = Some(crc32);

        Poll::Ready(Ok(crc32))
    }
}

#[derive(Debug)]
enum Chunk {
    LocalHeader { entry_index: usize },
    FileData { entry_index: usize },
    DataDescriptor { entry_index: usize },
    CDFileHeader { entry_index: usize },
    Eocd,
    Finished,
}

impl Chunk {
    fn new<D: EntryData>(entries: &[ReaderEntry<D>]) -> Chunk {
        if entries.is_empty() {
            Chunk::Eocd
        } else {
            Chunk::LocalHeader { entry_index: 0 }
        }
    }

    fn size<D: EntryData>(&self, entries: &[ReaderEntry<D>]) -> u64 {
        match self {
            Chunk::LocalHeader { entry_index } => {
                structs::LocalFileHeader::packed_size()
                    + entries[*entry_index].name.len() as u64
                    + structs::Zip64ExtraField::packed_size()
            }
            Chunk::FileData { entry_index } => entries[*entry_index].data.size(),
            Chunk::DataDescriptor { entry_index: _ } => structs::DataDescriptor64::packed_size(),
            Chunk::CDFileHeader { entry_index } => {
                structs::CentralDirectoryHeader::packed_size()
                    + entries[*entry_index].name.len() as u64
                    + structs::Zip64ExtraField::packed_size()
            }
            Chunk::Eocd => {
                structs::Zip64EndOfCentralDirectoryLocator::packed_size()
                    + structs::Zip64EndOfCentralDirectoryRecord::packed_size()
                    + structs::EndOfCentralDirectory::packed_size()
            }
            Chunk::Finished => unreachable!(),
        }
    }

    fn next<D: EntryData>(&self, entries: &[ReaderEntry<D>]) -> Chunk {
        match self {
            Chunk::LocalHeader { entry_index } => Chunk::FileData {
                entry_index: *entry_index,
            },
            Chunk::FileData { entry_index } => Chunk::DataDescriptor {
                entry_index: *entry_index,
            },
            Chunk::DataDescriptor { entry_index } => {
                let entry_index = *entry_index + 1;
                if entry_index < entries.len() {
                    Chunk::LocalHeader { entry_index }
                } else {
                    Chunk::CDFileHeader { entry_index: 0 }
                }
            }
            Chunk::CDFileHeader { entry_index } => {
                let entry_index = *entry_index + 1;
                if entry_index < entries.len() {
                    Chunk::CDFileHeader { entry_index }
                } else {
                    Chunk::Eocd
                }
            }
            Chunk::Eocd => Chunk::Finished,
            Chunk::Finished => unreachable!(),
        }
    }
}

#[pin_project(project = ReaderPinnedProj)]
enum ReaderPinned<D: EntryData> {
    Nothing,
    ReaderFuture(#[pin] D::ReaderFuture),
    FileReader(#[pin] CrcReader<D::Reader>),
}

/// Parts of the state of reader that don't need pinning.
/// As a result, these can be accessed using a mutable reference
/// and can have mutable methods
struct ReadState {
    /// Which chunk we are currently reading
    current_chunk: Chunk,
    /// How many bytes did we already read from the current chunk.
    /// Data in staging buffer counts as already processed.
    /// Used only for verification and for error reporting on EntryData.
    chunk_processed_size: u64,
    /// Current position in the file (counts bytes written to the output)
    /// This value is returned by tell.
    position: u64,
    /// Buffer for data that couldn't get written to the output directly.
    /// Content of this will get written to output first, before any more chunk
    /// reading is attempted.
    staging_buffer: Vec<u8>,
    /// How many bytes must be skipped, counted from the start of the current chunk
    to_skip: u64,
}

#[pin_project]
pub struct Reader<D: EntryData> {
    /// Sizes of central directory and whole zip.
    /// These should be immutable dring the Reader lifetime
    sizes: Sizes,

    /// Vector of entries and their offsets (counted from start of file)
    /// CRCs may get mutated during reading
    entries: Vec<ReaderEntry<D>>,

    /// Parts of mutable state that don't need pinning
    read_state: ReadState,

    /// Nested futures that need to be kept pinned, also used as a secondary state,
    #[pin]
    pinned: ReaderPinned<D>,
}

#[derive(Debug)]
struct Sizes {
    cd_offset: u64,
    cd_size: u64,
    eocd_offset: u64,
    total_size: u64,
}

impl ReadState {
    /// Read packed struct.
    /// Either reads to `output` (fast path), or to `self.staging_buffer`
    /// Never writes to output, if staging buffer is non empty, or if we need to skip something
    #[allow(clippy::needless_pass_by_value)] // Makes the call sites more ergonomic
    fn read_packed_struct<P>(&mut self, ps: P, output: &mut ReadBuf<'_>)
    where
        P: PackedStruct,
    {
        let size64 = P::packed_size();
        let size = P::packed_size_usize();
        self.chunk_processed_size += size64;

        if self.staging_buffer.is_empty() {
            if self.to_skip >= size64 {
                self.to_skip -= size64;
                return;
            } else if (self.to_skip == 0u64) & (output.remaining() >= size) {
                let output_slice = output.initialize_unfilled_to(size);
                ps.pack_to_slice(output_slice)
                    .unwrap_or_else(|_| unreachable!());
                output.advance(size);
                return;
            }
        }

        let buf_index = self.staging_buffer.len();
        self.staging_buffer.resize(buf_index + size, 0);
        ps.pack_to_slice(&mut self.staging_buffer[buf_index..])
            .unwrap_or_else(|_| {
                unreachable!(
                    "Buffer is resized appropriately, there is no other way this could fail"
                )
            });
    }

    /// Read string slice to the output or to the staging buffer
    /// Never writes to output, if staging buffer is non empty, or if we need to skip something
    fn read_str(&mut self, s: &str, output: &mut ReadBuf<'_>) {
        let bytes = s.as_bytes();
        let len64 = bytes.len() as u64;
        self.chunk_processed_size += len64;

        if self.staging_buffer.is_empty() {
            if self.to_skip >= len64 {
                self.to_skip -= len64;
                return;
            } else if (self.to_skip == 0u64) & (output.remaining() >= bytes.len()) {
                output.put_slice(bytes);
                return;
            }
        }
        self.staging_buffer.extend_from_slice(bytes);
    }

    /// Read from the staging buffer to output
    fn read_from_staging(&mut self, output: &mut ReadBuf<'_>) {
        let start = self.to_skip.try_into()
            .unwrap_or_else(|_|
                unreachable!(
                    "At most one chunk (but never a file data chunk) will ever be in the staging buffer, and those all are small."
                )
            );
        let end = start + output.remaining();
        let last_read = end >= self.staging_buffer.len();
        let end = if last_read {
            self.staging_buffer.len()
        } else {
            end
        };

        if start < self.staging_buffer.len() {
            output.put_slice(&self.staging_buffer[start..end]);
        }

        if last_read {
            self.to_skip = 0;
            self.staging_buffer.clear();
        } else {
            self.to_skip = end as u64;
        }
    }

    fn read_local_header<D: EntryData>(
        &mut self,
        entry: &ReaderEntry<D>,
        output: &mut ReadBuf<'_>,
    ) -> bool {
        self.read_packed_struct(
            structs::LocalFileHeader {
                signature: structs::LocalFileHeader::SIGNATURE,
                version_to_extract: ZIP64_VERSION_TO_EXTRACT.into(),
                flags: structs::GpBitFlag {
                    use_data_descriptor: true,
                },
                compression: structs::Compression::Store,
                last_mod_time: 0,
                last_mod_date: 0,
                crc32: 0,
                compressed_size: u32::MAX,
                uncompressed_size: u32::MAX,
                file_name_len: entry
                    .name
                    .len()
                    .try_into()
                    .unwrap_or_else(|_| unreachable!("Checked when constructing entries.")),
                extra_field_len: structs::Zip64ExtraField::packed_size_u16(),
            },
            output,
        );
        self.read_str(&entry.name, output);
        self.read_packed_struct(
            structs::Zip64ExtraField {
                tag: structs::Zip64ExtraField::TAG,
                size: structs::Zip64ExtraField::packed_size_u16() - 4,
                uncompressed_size: 0,
                compressed_size: 0,
                offset: entry.offset,
            },
            output,
        );

        true
    }

    fn read_file_data<D: EntryData>(
        &mut self,
        entry: &mut ReaderEntry<D>,
        mut pinned: Pin<&mut ReaderPinned<D>>,
        ctx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<Result<bool>> {
        let expected_size = entry.data.size();

        let mut file_reader = ready!(entry.get_reader(pinned.as_mut(), ctx))?;

        /*while self.to_skip > 0 {
            // Construct a temporary output buffer in the unused part of the real output buffer,
            // but not large enough to read more than the ammount to skip
            // TODO: Wouldn't it be better to use the staging buffer for this?
            let mut tmp_output = output.take(self.to_skip.try_into().unwrap_or(usize::MAX));
            assert!(tmp_output.filled().is_empty());

            ready!(file_reader.as_mut().poll_read(ctx, &mut tmp_output))?;

            self.chunk_processed_size += tmp_output.filled().len() as u64;
            self.to_skip -= tmp_output.filled().len() as u64;
        }*/
        // TODO: We might want to decide to recompute the CRC instead
        if self.to_skip > 0 {
            let pos_after_seek = ready!(file_reader
                .as_mut()
                .seek(ctx, SeekFrom::Start(self.to_skip)))?;
            assert!(pos_after_seek == self.to_skip);
            self.chunk_processed_size += self.to_skip;
            self.to_skip = 0;
        }

        let remaining_before_poll = output.remaining();
        ready!(file_reader.as_mut().poll_read(ctx, output))?;

        if output.remaining() == remaining_before_poll {
            // Nothing was output => we read everything in the file already

            if file_reader.is_crc_valid() {
                entry.crc32 = Some(file_reader.get_crc32());
            }

            pinned.set(ReaderPinned::Nothing);

            if self.chunk_processed_size == expected_size {
                Poll::Ready(Ok(true)) // We're done with this state
            } else {
                Poll::Ready(Err(Error::other(ZippityError::LengthMismatch {
                    entry_name: entry.name.clone(),
                    expected_size,
                    actual_size: self.chunk_processed_size,
                })))
            }
        } else {
            let written_chunk_size = remaining_before_poll - output.remaining();
            let buf_slice = output.filled();
            let written_chunk = &buf_slice[(buf_slice.len() - written_chunk_size)..];
            assert!(written_chunk_size == written_chunk.len());

            self.chunk_processed_size += written_chunk_size as u64;

            Poll::Ready(Ok(false))
        }
    }

    fn read_data_descriptor<D: EntryData>(
        &mut self,
        entry: &mut ReaderEntry<D>,
        pinned: Pin<&mut ReaderPinned<D>>,
        ctx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<Result<bool>> {
        let crc32 = ready!(entry.get_crc(pinned, ctx, &mut self.staging_buffer))?;

        self.read_packed_struct(
            structs::DataDescriptor64 {
                signature: structs::DataDescriptor64::SIGNATURE,
                crc32,
                compressed_size: entry.data.size(),
                uncompressed_size: entry.data.size(),
            },
            output,
        );

        Poll::Ready(Ok(true))
    }

    fn read_cd_file_header<D: EntryData>(
        &mut self,
        entry: &mut ReaderEntry<D>,
        pinned: Pin<&mut ReaderPinned<D>>,
        ctx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<Result<bool>> {
        let crc32 = ready!(entry.get_crc(pinned, ctx, &mut self.staging_buffer))?;

        self.read_packed_struct(
            structs::CentralDirectoryHeader {
                signature: structs::CentralDirectoryHeader::SIGNATURE,
                version_made_by: structs::VersionMadeBy {
                    os: structs::VersionMadeByOs::Unix,
                    spec_version: ZIP64_VERSION_TO_EXTRACT,
                },
                version_to_extract: ZIP64_VERSION_TO_EXTRACT.into(),
                flags: 0,
                compression: structs::Compression::Store,
                last_mod_time: 0,
                last_mod_date: 0,
                crc32,
                compressed_size: u32::MAX,
                uncompressed_size: u32::MAX,
                file_name_len: entry
                    .name
                    .len()
                    .try_into()
                    .unwrap_or_else(|_| unreachable!("Checked when constructing entries.")),
                extra_field_len: structs::Zip64ExtraField::packed_size_u16(),
                file_comment_length: 0,
                disk_number_start: u16::MAX,
                internal_attributes: 0,
                external_attributes: 0,
                local_header_offset: u32::MAX,
            },
            output,
        );
        self.read_str(&entry.name, output);
        self.read_packed_struct(
            structs::Zip64ExtraField {
                tag: structs::Zip64ExtraField::TAG,
                size: structs::Zip64ExtraField::packed_size_u16() - 4,
                uncompressed_size: entry.data.size(),
                compressed_size: entry.data.size(),
                offset: entry.offset,
            },
            output,
        );

        Poll::Ready(Ok(true))
    }

    fn read_eocd<D: EntryData>(
        &mut self,
        sizes: &Sizes,
        entries: &[ReaderEntry<D>],
        output: &mut ReadBuf<'_>,
    ) -> bool {
        self.read_packed_struct(
            structs::Zip64EndOfCentralDirectoryRecord {
                signature: structs::Zip64EndOfCentralDirectoryRecord::SIGNATURE,
                size_of_zip64_eocd: structs::Zip64EndOfCentralDirectoryRecord::packed_size() - 12,
                version_made_by: structs::VersionMadeBy {
                    os: structs::VersionMadeByOs::Unix,
                    spec_version: ZIP64_VERSION_TO_EXTRACT,
                },
                version_to_extract: ZIP64_VERSION_TO_EXTRACT.into(),
                this_disk_number: 0,
                start_of_cd_disk_number: 0,
                this_cd_entry_count: entries.len() as u64,
                total_cd_entry_count: entries.len() as u64,
                size_of_cd: sizes.cd_size,
                cd_offset: sizes.cd_offset,
            },
            output,
        );
        self.read_packed_struct(
            structs::Zip64EndOfCentralDirectoryLocator {
                signature: structs::Zip64EndOfCentralDirectoryLocator::SIGNATURE,
                start_of_cd_disk_number: 0,
                zip64_eocd_offset: sizes.eocd_offset,
                number_of_disks: 1,
            },
            output,
        );
        self.read_packed_struct(
            structs::EndOfCentralDirectory {
                signature: structs::EndOfCentralDirectory::SIGNATURE,
                this_disk_number: 0,
                start_of_cd_disk_number: 0,
                this_cd_entry_count: u16::MAX,
                total_cd_entry_count: u16::MAX,
                size_of_cd: u32::MAX,
                cd_offset: u32::MAX,
                file_comment_length: 0,
            },
            output,
        );

        true
    }

    fn read<D: EntryData>(
        &mut self,
        sizes: &Sizes,
        entries: &mut [ReaderEntry<D>],
        mut pinned: Pin<&mut ReaderPinned<D>>,
        ctx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let initial_remaining = output.remaining();

        while output.remaining() > 0 {
            if !self.staging_buffer.is_empty() {
                self.read_from_staging(output);
                continue;
            }

            if let Chunk::Finished = self.current_chunk {
                return Poll::Ready(Ok(()));
            }

            let current_chunk_size = self.current_chunk.size(entries);
            if self.to_skip >= current_chunk_size {
                self.to_skip -= current_chunk_size;
                self.current_chunk = self.current_chunk.next(entries);
                continue;
            }

            let chunk_done = match &mut self.current_chunk {
                Chunk::LocalHeader { entry_index } => {
                    let entry_index = *entry_index;
                    self.read_local_header(&entries[entry_index], output)
                }
                Chunk::FileData { entry_index } => {
                    let entry_index = *entry_index;
                    if output.remaining() != initial_remaining {
                        // We have already written something into the output -> interrupt this call, because
                        // we might need to return Pending when reading the file data
                        // TODO: Make sure that there is nothing in the staging buffer as well?
                        break;
                    }
                    let read_result = self.read_file_data(
                        &mut entries[entry_index],
                        pinned.as_mut(),
                        ctx,
                        output,
                    );
                    ready!(read_result)?
                }
                Chunk::DataDescriptor { entry_index } => {
                    let entry_index = *entry_index;
                    ready!(self.read_data_descriptor(
                        &mut entries[entry_index],
                        pinned.as_mut(),
                        ctx,
                        output
                    ))?
                }
                Chunk::CDFileHeader { entry_index } => {
                    let entry_index = *entry_index;
                    ready!(self.read_cd_file_header(
                        &mut entries[entry_index],
                        pinned.as_mut(),
                        ctx,
                        output
                    ))?
                }
                Chunk::Eocd => self.read_eocd(sizes, entries, output),
                Chunk::Finished => unreachable!(),
            };

            if chunk_done {
                assert!(self.chunk_processed_size == current_chunk_size);
                self.current_chunk = self.current_chunk.next(entries);
                self.chunk_processed_size = 0;
            }
        }

        self.position += (initial_remaining - output.remaining()) as u64;
        Poll::Ready(Ok(()))
    }

    /// Always succeeds, seeking past end of file causes `tell()` to return the
    /// set position and reads returning empty buffers (=EOF).
    fn seek_bytes<D: EntryData>(
        &mut self,
        sizes: &Sizes,
        entries: &[ReaderEntry<D>],
        mut pinned: Pin<&mut ReaderPinned<D>>,
        position: u64,
    ) {
        let (chunk, chunk_position) = if position >= sizes.eocd_offset {
            (Chunk::Eocd, sizes.eocd_offset)
        } else if position >= sizes.cd_offset {
            assert!(!entries.is_empty());
            (Chunk::CDFileHeader { entry_index: 0 }, sizes.cd_offset)
        } else {
            assert!(!entries.is_empty());
            let entry_index = match entries.binary_search_by_key(&position, |entry| entry.offset) {
                Ok(index) => index,
                Err(index) => index - 1,
            };
            assert!(entry_index < entries.len());
            assert!(entries[entry_index].offset <= position);
            assert!(entry_index == entries.len() - 1 || entries[entry_index + 1].offset > position);
            (
                Chunk::LocalHeader { entry_index },
                entries[entry_index].offset,
            )
        };

        self.current_chunk = chunk;
        self.chunk_processed_size = 0;
        self.position = position;
        self.staging_buffer.clear();
        self.to_skip = position - chunk_position;

        pinned.set(ReaderPinned::Nothing);
    }
}

impl<D: EntryData> Reader<D> {
    /// Return the total size of the file in bytes.
    pub fn size(&self) -> u64 {
        self.sizes.total_size
    }

    /// Return current position in the ZIP file in bytes.
    /// If not seeking, this is the number of bytes already read.
    pub fn tell(&self) -> u64 {
        self.read_state.position
    }

    /// Returns an iterator of already calculated entry CRC32s.
    /// This can be used to cache the CRCs externally to speed up later
    /// requests that seek within the file (not having to re-read the whole
    /// entry data when only CRC is needed).
    pub fn crc32s(&self) -> impl Iterator<Item = (&str, u32)> {
        self.entries
            .iter()
            .filter_map(|entry| entry.crc32.map(|crc| (entry.name.as_str(), crc)))
    }
}

impl<D: EntryData> AsyncRead for Reader<D> {
    fn poll_read(
        self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<Result<()>> {
        let projected = self.project();
        projected.read_state.read(
            projected.sizes,
            projected.entries.as_mut_slice(),
            projected.pinned,
            ctx,
            buf,
        )
    }
}

impl<D: EntryData> AsyncSeek for Reader<D> {
    fn start_seek(self: Pin<&mut Self>, position: SeekFrom) -> Result<()> {
        let resolve_offset = |base: u64, offset: i64| -> Result<u64> {
            if let Some(result) = base.checked_add_signed(offset) {
                if result <= self.size() {
                    return Ok(result);
                }
            }

            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                ZippityError::SeekingOutsideOfFile,
            ))
        };
        let pos = match position {
            SeekFrom::Start(pos) => pos,
            SeekFrom::Current(offset) => resolve_offset(self.tell(), offset)?,
            SeekFrom::End(offset) => resolve_offset(self.size(), offset)?,
        };

        let projected = self.project();
        projected
            .read_state
            .seek_bytes(projected.sizes, projected.entries, projected.pinned, pos);
        // TODO: This way the seek is lazy, meaning that it will prefer not to do anything
        // until there is a time to actually read. Is this ok?
        Ok(())
    }

    fn poll_complete(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<u64>> {
        Poll::Ready(Ok(self.tell()))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::test_util::{measure_size, read_size_strategy, read_to_vec, ZerosReader};
    use assert2::assert;
    use assert_matches::assert_matches;
    use proptest::strategy::Strategy;
    use std::{collections::HashMap, io::ErrorKind, pin::pin};
    use test_case::test_case;
    use test_strategy::proptest;
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    use tokio_test::block_on;
    use zip::read::ZipArchive;

    fn content_strategy() -> impl Strategy<Value = HashMap<String, Vec<u8>>> {
        proptest::collection::hash_map(
            ".*",
            proptest::collection::vec(proptest::bits::u8::ANY, 0..100),
            0..100,
        )
    }

    use packed_struct::prelude::*;
    #[derive(Debug, PackedStruct)]
    #[packed_struct(endian = "msb")]
    struct TestPs {
        v: u64,
    }

    impl TestPs {
        fn new() -> TestPs {
            TestPs {
                v: 0x0102030405060708,
            }
        }
    }

    #[test]
    fn too_long_enty_name() {
        let mut builder: Builder<()> = Builder::new();

        let name_length = u16::MAX as usize + 1;
        let e = builder
            .add_entry(
                std::iter::repeat("X").take(name_length).collect::<String>(),
                (),
            )
            .unwrap_err();
        assert_matches!(e, ZippityError::TooLongEntryName { entry_name } if entry_name.len() == name_length);
    }

    #[proptest]
    fn read_packed_struct_direct_write(
        #[strategy(proptest::collection::vec(proptest::bits::u8::ANY, 0..10))]
        initial_output_content: Vec<u8>,
        #[strategy(0usize..20usize)] output_buffer_extra_size: usize,
    ) {
        let mut rs = ReadState {
            current_chunk: Chunk::Finished,
            chunk_processed_size: 0,
            staging_buffer: Vec::new(),
            to_skip: 0,
            position: 0,
        };

        let mut buf_backing = Vec::new();
        buf_backing.resize(
            initial_output_content.len() + output_buffer_extra_size + 8,
            0,
        );
        let mut buf = ReadBuf::new(buf_backing.as_mut_slice());
        buf.put_slice(initial_output_content.as_slice());

        assert!(
            buf.remaining() >= 8,
            "Test construction: There must be enough room for the whole output"
        );
        rs.read_packed_struct(TestPs::new(), &mut buf);

        assert!(
            &buf.filled()[..initial_output_content.len()] == initial_output_content.as_slice(),
            "Must not touch what was in the buffer before"
        );
        assert!(
            &buf.filled()[initial_output_content.len()..]
                == &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08],
            "Must write the whole struct to output"
        );
    }

    #[proptest]
    fn read_packed_struct_skip_all(
        #[strategy(proptest::collection::vec(proptest::bits::u8::ANY, 0..10))]
        initial_output_content: Vec<u8>,
        #[strategy(0usize..20usize)] output_buffer_extra_size: usize,
        #[strategy(8u64..100u64)] to_skip: u64,
    ) {
        let mut rs = ReadState {
            current_chunk: Chunk::Finished,
            chunk_processed_size: 0,
            staging_buffer: Vec::new(),
            to_skip,
            position: 0,
        };

        let mut buf_backing = Vec::new();
        buf_backing.resize(initial_output_content.len() + output_buffer_extra_size, 0);
        let mut buf = ReadBuf::new(buf_backing.as_mut_slice());
        buf.put_slice(initial_output_content.as_slice());

        rs.read_packed_struct(TestPs::new(), &mut buf);

        assert!(
            buf.filled() == initial_output_content.as_slice(),
            "Must not touch what was in the buffer before"
        );
        assert!(rs.to_skip == to_skip - 8, "Must decrease amount to skip");
    }

    #[proptest]
    fn read_packed_struct_skip_partial(
        #[strategy(proptest::collection::vec(proptest::bits::u8::ANY, 0..10))]
        initial_output_content: Vec<u8>,
        #[strategy(0usize..20usize)] output_buffer_extra_size: usize,
        #[strategy(1u64..8u64)] to_skip: u64,
    ) {
        let mut rs = ReadState {
            current_chunk: Chunk::Finished,
            chunk_processed_size: 0,
            staging_buffer: Vec::new(),
            to_skip,
            position: 0,
        };

        let mut buf_backing = Vec::new();
        buf_backing.resize(
            initial_output_content.len() + output_buffer_extra_size + 1, // Always have room for at least 1 byte to read
            0,
        );
        let mut buf = ReadBuf::new(buf_backing.as_mut_slice());
        buf.put_slice(initial_output_content.as_slice());

        rs.read_packed_struct(TestPs::new(), &mut buf);

        assert!(
            buf.filled() == initial_output_content.as_slice(),
            "Must not touch what was in the buffer before"
        );
        assert!(
            rs.to_skip == to_skip,
            "Must not change how much there was to skip"
        );
        assert!(
            rs.staging_buffer.as_slice() == &[1, 2, 3, 4, 5, 6, 7, 8],
            "Must copy the whole structure to buffer"
        );
    }

    #[proptest]
    fn read_packed_struct_nonempty_buffer(
        #[strategy(proptest::collection::vec(proptest::bits::u8::ANY, 1..10))]
        initial_buffer_content: Vec<u8>,
        #[strategy(proptest::collection::vec(proptest::bits::u8::ANY, 0..10))]
        initial_output_content: Vec<u8>,
        #[strategy(0usize..20usize)] output_buffer_extra_size: usize,
        #[strategy(0f64..=1f64)] to_skip_f: f64,
    ) {
        let to_skip = (initial_buffer_content.len() as f64 * to_skip_f) as u64;
        let mut rs = ReadState {
            current_chunk: Chunk::Finished,
            chunk_processed_size: 0,
            staging_buffer: initial_buffer_content.clone(),
            to_skip,
            position: 0,
        };

        let mut buf_backing = Vec::new();
        buf_backing.resize(
            initial_output_content.len() + output_buffer_extra_size + 1, // Always have room for at least 1 byte to read
            0,
        );
        let mut buf = ReadBuf::new(buf_backing.as_mut_slice());
        buf.put_slice(initial_output_content.as_slice());

        rs.read_packed_struct(TestPs::new(), &mut buf);

        assert!(
            buf.filled() == initial_output_content.as_slice(),
            "Must not touch what was in the buffer before"
        );
        assert!(
            rs.to_skip == to_skip,
            "Must not change how much there was to skip"
        );
        assert!(
            &rs.staging_buffer[..initial_buffer_content.len()] == initial_buffer_content.as_slice(),
            "Must not modify the initial content of the buffer"
        );
        assert!(
            &rs.staging_buffer[initial_buffer_content.len()..] == [1, 2, 3, 4, 5, 6, 7, 8],
            "Must append the packed structure to buffer"
        );
    }

    #[proptest]
    fn read_str_direct_write(
        #[strategy(".*")] test_data: String,
        #[strategy(proptest::collection::vec(proptest::bits::u8::ANY, 0..10))]
        initial_output_content: Vec<u8>,
        #[strategy(0usize..20usize)] output_buffer_extra_size: usize,
    ) {
        let mut rs = ReadState {
            current_chunk: Chunk::Finished,
            chunk_processed_size: 0,
            staging_buffer: Vec::new(),
            to_skip: 0,
            position: 0,
        };

        let mut buf_backing = Vec::new();
        buf_backing.resize(
            initial_output_content.len() + output_buffer_extra_size + test_data.len(),
            0,
        );
        let mut buf = ReadBuf::new(buf_backing.as_mut_slice());
        buf.put_slice(initial_output_content.as_slice());

        assert!(
            buf.remaining() >= test_data.len(),
            "Test construction: There must be enough room for the whole output"
        );
        rs.read_str(&test_data, &mut buf);

        assert!(
            &buf.filled()[..initial_output_content.len()] == initial_output_content.as_slice(),
            "Must not touch what was in the buffer before"
        );
        assert!(
            &buf.filled()[initial_output_content.len()..] == test_data.as_bytes(),
            "Must write the whole string to output"
        );
    }

    #[proptest]
    fn read_str_skip_all(
        #[strategy(".*")] test_data: String,
        #[strategy(proptest::collection::vec(proptest::bits::u8::ANY, 0..10))]
        initial_output_content: Vec<u8>,
        #[strategy(0usize..20usize)] output_buffer_extra_size: usize,
        #[strategy(0u64..100u64)] to_skip_extra: u64,
    ) {
        let to_skip = test_data.len() as u64 + to_skip_extra;
        let mut rs = ReadState {
            current_chunk: Chunk::Finished,
            chunk_processed_size: 0,
            staging_buffer: Vec::new(),
            to_skip,
            position: 0,
        };

        let mut buf_backing = Vec::new();
        buf_backing.resize(initial_output_content.len() + output_buffer_extra_size, 0);
        let mut buf = ReadBuf::new(buf_backing.as_mut_slice());
        buf.put_slice(initial_output_content.as_slice());

        rs.read_str(&test_data, &mut buf);

        assert!(
            buf.filled() == initial_output_content.as_slice(),
            "Must not touch what was in the buffer before"
        );
        assert!(rs.to_skip == to_skip_extra, "Must decrease amount to skip");
    }

    #[proptest]
    fn read_str_skip_partial(
        #[strategy("...*")] test_data: String, // At least two characters
        #[strategy(proptest::collection::vec(proptest::bits::u8::ANY, 0..10))]
        initial_output_content: Vec<u8>,
        #[strategy(0usize..20usize)] output_buffer_extra_size: usize,
        #[strategy(0f64..1f64)] to_skip_f: f64,
    ) {
        let to_skip = 1 + ((test_data.len() - 2) as f64 * to_skip_f) as u64;
        assert!(
            to_skip > 0,
            "Test construction: We always must skip at least 1 byte"
        );
        assert!(
            to_skip < test_data.len() as u64,
            "Test construction: We always must leave at least 1 byte to write"
        );
        let mut rs = ReadState {
            current_chunk: Chunk::Finished,
            chunk_processed_size: 0,
            staging_buffer: Vec::new(),
            to_skip,
            position: 0,
        };

        let mut buf_backing = Vec::new();
        buf_backing.resize(
            initial_output_content.len() + output_buffer_extra_size + 1, // Always have room for at least 1 byte to read
            0,
        );
        let mut buf = ReadBuf::new(buf_backing.as_mut_slice());
        buf.put_slice(initial_output_content.as_slice());

        rs.read_str(&test_data, &mut buf);

        assert!(
            buf.filled() == initial_output_content.as_slice(),
            "Must not touch what was in the buffer before"
        );
        assert!(
            rs.to_skip == to_skip,
            "Must not change how much there was to skip"
        );
        assert!(
            rs.staging_buffer.as_slice() == test_data.as_bytes(),
            "Must copy the whole string to buffer"
        );
    }

    #[proptest]
    fn read_str_nonempty_buffer(
        #[strategy(".*")] test_data: String,
        #[strategy(proptest::collection::vec(proptest::bits::u8::ANY, 1..10))]
        initial_buffer_content: Vec<u8>,
        #[strategy(proptest::collection::vec(proptest::bits::u8::ANY, 0..10))]
        initial_output_content: Vec<u8>,
        #[strategy(0usize..20usize)] output_buffer_extra_size: usize,
        #[strategy(0f64..=1f64)] to_skip_f: f64,
    ) {
        let to_skip = (initial_buffer_content.len() as f64 * to_skip_f) as u64;
        let mut rs = ReadState {
            current_chunk: Chunk::Finished,
            chunk_processed_size: 0,
            staging_buffer: initial_buffer_content.clone(),
            to_skip,
            position: 0,
        };

        let mut buf_backing = Vec::new();
        buf_backing.resize(
            initial_output_content.len() + output_buffer_extra_size + 1, // Always have room for at least 1 byte to read
            0,
        );
        let mut buf = ReadBuf::new(buf_backing.as_mut_slice());
        buf.put_slice(initial_output_content.as_slice());

        rs.read_str(&test_data, &mut buf);

        assert!(
            buf.filled() == initial_output_content.as_slice(),
            "Must not touch what was in the buffer before"
        );
        assert!(
            rs.to_skip == to_skip,
            "Must not change how much there was to skip"
        );
        assert!(
            &rs.staging_buffer[..initial_buffer_content.len()] == initial_buffer_content.as_slice(),
            "Must not modify the initial content of the buffer"
        );
        assert!(
            &rs.staging_buffer[initial_buffer_content.len()..] == test_data.as_bytes(),
            "Must append the packed structure to buffer"
        );
    }

    #[proptest]
    fn read_from_staging_works_as_expected(
        #[strategy(proptest::collection::vec(proptest::bits::u8::ANY, 1..100))] test_data: Vec<u8>,
        #[strategy(proptest::collection::vec(proptest::bits::u8::ANY, 0..10))]
        initial_output_content: Vec<u8>,
        #[strategy(1u64..100u64)] to_skip: u64,
        #[strategy(1usize..100usize)] read_size: usize,
    ) {
        let mut rs = ReadState {
            current_chunk: Chunk::Finished,
            chunk_processed_size: 0,
            staging_buffer: test_data.clone(),
            to_skip,
            position: 0,
        };

        // Calculate range in test data that will be added to the output.
        let start = to_skip as usize;
        let stop = (start + read_size).min(test_data.len());
        let start = start.min(stop);

        let mut buf_backing = vec![0; initial_output_content.len() + read_size];
        let mut buf = ReadBuf::new(buf_backing.as_mut_slice());
        buf.put_slice(initial_output_content.as_slice());

        rs.read_from_staging(&mut buf);

        assert!(
            &buf.filled()[..initial_output_content.len()] == initial_output_content.as_slice(),
            "Must not touch what was in the staging buffer before"
        );
        assert!(
            &buf.filled()[initial_output_content.len()..] == &test_data[start..stop],
            "Must write the content of the staging buffer minus the skipped data to output"
        );
        if stop >= test_data.len() {
            assert!(
                rs.staging_buffer.len() == 0,
                "Must clear the staging buffer if all data was read"
            );
        } else {
            assert!(&rs.staging_buffer[rs.to_skip as usize..] == &test_data[stop..],
            "What remains in the staging buffer after the skip must be what we didn't write from the test data");
        }
    }

    #[test]
    fn empty_archive_can_be_unzipped() {
        let zippity = pin!(Builder::<()>::new().build());
        let size = zippity.size();

        let buf = block_on(read_to_vec(zippity, 1)).unwrap();

        assert!(size == (buf.len() as u64));

        let unpacked = ZipArchive::new(std::io::Cursor::new(buf)).expect("Should be a valid zip");
        assert!(unpacked.is_empty());
    }

    #[test]
    fn empty_entry_name_can_be_unzipped() {
        let mut builder: Builder<()> = Builder::new();

        builder.add_entry(String::new(), ()).unwrap();

        let zippity = pin!(builder.build());
        let buf = block_on(read_to_vec(zippity, 8192)).unwrap();

        let mut unpacked =
            ZipArchive::new(std::io::Cursor::new(buf)).expect("Should be a valid zip");
        assert!(unpacked.len() == 1);

        let mut zipfile = unpacked.by_index(0).unwrap();
        let name = std::str::from_utf8(zipfile.name_raw()).unwrap().to_string();
        assert!(name.is_empty());
        let mut file_content = Vec::new();
        use std::io::Read;
        zipfile.read_to_end(&mut file_content).unwrap();
        assert!(file_content.is_empty());
    }

    #[test]
    fn archive_with_single_file_can_be_unzipped() {
        let mut builder: Builder<&[u8]> = Builder::new();

        builder
            .add_entry("Foo".to_owned(), b"bar!".as_slice())
            .unwrap();

        let zippity = pin!(builder.build());
        let size = zippity.size();

        let buf = block_on(read_to_vec(zippity, 1)).unwrap();

        assert!(size == (buf.len() as u64));

        let mut unpacked =
            ZipArchive::new(std::io::Cursor::new(buf)).expect("Should be a valid zip");
        assert!(unpacked.len() == 1);

        let mut zipfile = unpacked.by_index(0).unwrap();
        let name = std::str::from_utf8(zipfile.name_raw()).unwrap().to_string();
        assert!(name == "Foo");
        let mut file_content = Vec::new();
        use std::io::Read;
        zipfile.read_to_end(&mut file_content).unwrap();
        assert!(file_content == b"bar!");
    }

    #[test]
    fn archive_with_single_empty_file_can_be_unzipped() {
        let mut builder: Builder<&[u8]> = Builder::new();

        builder.add_entry("0".to_owned(), b"".as_slice()).unwrap();

        let zippity = pin!(builder.build());
        let size = zippity.size();

        let buf = block_on(read_to_vec(zippity, 1)).unwrap();

        assert!(size == (buf.len() as u64));

        let mut unpacked =
            ZipArchive::new(std::io::Cursor::new(buf)).expect("Should be a valid zip");
        assert!(unpacked.len() == 1);

        let mut zipfile = unpacked.by_index(0).unwrap();
        let name = std::str::from_utf8(zipfile.name_raw()).unwrap().to_string();
        assert!(name == "0");
        let mut file_content = Vec::new();
        use std::io::Read;
        zipfile.read_to_end(&mut file_content).unwrap();
        assert!(file_content == b"");
    }

    #[proptest]
    fn result_can_be_unzipped(
        #[strategy(content_strategy())] content: HashMap<String, Vec<u8>>,
        #[strategy(read_size_strategy())] read_size: usize,
    ) {
        let mut builder: Builder<&[u8]> = Builder::new();

        content.iter().for_each(|(name, value)| {
            builder.add_entry(name.clone(), value.as_ref()).unwrap();
        });

        let zippity = pin!(builder.build());
        let size = zippity.size();

        let buf = block_on(read_to_vec(zippity, read_size)).unwrap();

        assert!(size == (buf.len() as u64));

        let mut unpacked =
            ZipArchive::new(std::io::Cursor::new(buf)).expect("Should be a valid zip");
        assert!(unpacked.len() == content.len());

        let mut unpacked_content = HashMap::new();
        for i in 0..unpacked.len() {
            dbg!(&i);
            let mut zipfile = unpacked.by_index(i).unwrap();
            let name = std::str::from_utf8(zipfile.name_raw()).unwrap().to_string();
            let mut file_content = Vec::new();
            use std::io::Read;
            zipfile.read_to_end(&mut file_content).unwrap();

            unpacked_content.insert(name, file_content);
        }
        assert!(unpacked_content == content);
    }

    /// Tests that a zip archive with large file (> 4GB) and many files (> 65536)
    /// can be created.
    ///
    /// Doesn't check that it unpacks correctly to keep the requirements low,
    /// only verifies that the expected and actual sizes match.
    /// This doesn't actually store the file or archive, but is still fairly slow.
    #[test]
    #[ignore = "this test is too slow"]
    fn zip64_works() {
        let mut builder = Builder::<ZerosReader>::new();
        builder
            .add_entry("Big file".to_owned(), ZerosReader::new(0x100000000))
            .unwrap();
        for i in 0..0xffff {
            builder
                .add_entry(format!("Empty file {}", i), ZerosReader::new(0))
                .unwrap();
        }
        let zippity = pin!(builder.build());

        let expected_size = zippity.size();
        let actual_size = block_on(measure_size(zippity)).unwrap();

        assert!(actual_size == expected_size);
    }

    #[proptest]
    fn seeking_returns_correct_data(
        #[strategy(content_strategy())] content: HashMap<String, Vec<u8>>,
        #[strategy(read_size_strategy())] read_size: usize,
        #[strategy(0f64..=1f64)] seek_pos_fraction: f64,
    ) {
        let mut builder: Builder<&[u8]> = Builder::new();
        content.iter().for_each(|(name, value)| {
            builder.add_entry(name.clone(), value.as_ref()).unwrap();
        });

        let zippity_whole = pin!(builder.clone().build());
        let size = zippity_whole.size();

        dbg!(size);

        let buf_whole = block_on(read_to_vec(zippity_whole, 8192)).unwrap();
        assert!(size == (buf_whole.len() as u64));

        let seek_pos = (seek_pos_fraction * size as f64).floor() as u64;
        dbg!(seek_pos);
        assert!(
            seek_pos < size,
            "Test construction: The position must be inside the file"
        );

        let mut zippity_part = pin!(builder.build());
        let seek_reported_position =
            block_on(zippity_part.seek(SeekFrom::Start(seek_pos))).unwrap();
        assert!(seek_reported_position == seek_pos);
        let buf_part = block_on(read_to_vec(zippity_part, read_size)).unwrap();

        assert!(buf_part == buf_whole[seek_pos as usize..]);
    }

    /// Test that reading multiple single bytes inside the zip file with absolute seeks
    /// in between returns the same bytes as reading the whole file.
    /// Also tests reusing the zippity reader object.
    #[proptest]
    fn seeking_single_bytes(
        #[strategy(content_strategy())] content: HashMap<String, Vec<u8>>,
        #[strategy(proptest::collection::vec(0f64..=1f64, 1..100))] byte_positions_f: Vec<f64>,
    ) {
        let mut builder: Builder<&[u8]> = Builder::new();
        content.iter().for_each(|(name, value)| {
            builder.add_entry(name.clone(), value.as_ref()).unwrap();
        });

        let mut zippity = pin!(builder.clone().build());
        let size = zippity.size();
        let buf_whole = block_on(read_to_vec(zippity.as_mut(), 8192)).unwrap();
        assert!(size == (buf_whole.len() as u64));

        dbg!(size);

        for fraction in byte_positions_f {
            let seek_pos = (fraction * size as f64).floor() as u64;
            dbg!(seek_pos);
            assert!(
                seek_pos < size,
                "Test construction: The position must be inside the file"
            );
            let expected = buf_whole[seek_pos as usize];

            block_on(zippity.seek(SeekFrom::Start(seek_pos))).unwrap();
            let actual = block_on(zippity.read_u8()).unwrap();
            assert!(actual == expected);
        }
    }

    #[test]
    fn bad_size_entry_data_errors_out() {
        /// Struct that reports data size 100, but actually its 1
        struct BadSize();
        impl EntryData for BadSize {
            type Reader = std::io::Cursor<&'static [u8]>;
            type ReaderFuture = std::future::Ready<Result<Self::Reader>>;

            fn size(&self) -> u64 {
                100
            }

            fn get_reader(&self) -> Self::ReaderFuture {
                std::future::ready(Ok(std::io::Cursor::new(&[5])))
            }
        }

        let mut builder: Builder<BadSize> = Builder::new();
        builder.add_entry("xxx".into(), BadSize()).unwrap();

        let zippity = pin!(builder.build());
        let e = block_on(read_to_vec(zippity, 1024)).unwrap_err();

        assert!(e.kind() == ErrorKind::Other);
        let message = format!("{}", e.into_inner().unwrap());

        assert!(message.contains("xxx"));
    }

    /// Tests an internal property of the builder -- that the sizes generated
    /// during building actually match the chunk size.
    #[proptest]
    fn local_size_matches_chunks(
        #[strategy(content_strategy())] content: HashMap<String, Vec<u8>>,
    ) {
        let mut builder: Builder<&[u8]> = Builder::new();

        content.iter().for_each(|(name, value)| {
            builder.add_entry(name.clone(), value.as_ref()).unwrap();
        });

        let local_sizes: HashMap<String, u64> = builder
            .entries
            .iter()
            .map(|(k, v)| (k.clone(), v.get_local_size(k)))
            .collect();

        let zippity = builder.build();

        for i in 0..zippity.entries.len() {
            let mut entry_local_size = 0;
            let mut chunk = Chunk::LocalHeader { entry_index: i };
            loop {
                dbg!(&chunk);
                dbg!(chunk.size(&zippity.entries));
                entry_local_size += chunk.size(&zippity.entries);
                chunk = chunk.next(&zippity.entries);

                if let Chunk::LocalHeader { entry_index: _ } = chunk {
                    break;
                }
                if let Chunk::CDFileHeader { entry_index: _ } = chunk {
                    break;
                }
            }

            assert!(local_sizes[&zippity.entries[i].name] == entry_local_size);
        }
    }

    /// Gray box test that verifies behavior of CRC recalculation.
    /// Either seeks within file data (possibility to recalculate CRC when
    /// producing remaining data), or after the file data (has to be recalculated separately).
    /// Either pre-fills the CRC from ground truth run or not.
    /// In all cases compares the remainder of the seek with ground truth
    /// generated without seeking and without pre-filled CRC.
    #[test_case(false, false; "No cached CRC, seeking within data")]
    #[test_case(false, true; "No cached CRC, seeking past the data")]
    #[test_case(true, false; "Cached CRC, seeking within data")]
    #[test_case(true, true; "Cached CRC, seeking past the data")]
    fn crc_recalculation_with_seek(pre_fill_crc: bool, seek_past_data: bool) {
        let test_data: Vec<u8> = (0..(8192 + 1)).map(|v| (v & 0xff) as u8).collect();
        // The test data is larger than 8k so that we always have to loop more than once in the crc recalculation code

        let mut builder1 = Builder::<&[u8]>::new();
        builder1
            .add_entry("X".to_owned(), test_data.as_slice())
            .unwrap();

        let mut reader1 = pin!(builder1.build());
        let whole_zip = block_on(read_to_vec(reader1.as_mut(), 8192)).unwrap();

        let entry_crc = {
            let mut crc_it = reader1.crc32s();
            let pair = crc_it.next().expect("There should be exactly one item");
            assert!(crc_it.next() == None, "There should be exactly one item");
            assert!(
                pair.0 == "X",
                "The item recovered must have the expected name"
            );
            pair.1
        };

        let mut builder2 = Builder::<&[u8]>::new();
        let entry = builder2
            .add_entry("X".to_owned(), test_data.as_slice())
            .unwrap();
        if pre_fill_crc {
            entry.crc32(entry_crc);
        }
        let mut reader2 = pin!(builder2.build());

        if seek_past_data {
            block_on(reader2.as_mut().seek(SeekFrom::Start(
                test_data.len() as u64
                    + Chunk::LocalHeader { entry_index: 0 }.size(reader1.entries.as_slice()),
            )))
            .unwrap();
        } else {
            block_on(
                reader2
                    .as_mut()
                    .seek(SeekFrom::Start(test_data.len() as u64)),
            )
            .unwrap();
        }

        let zip_after_seek = block_on(read_to_vec(reader2.as_mut(), 8192)).unwrap();

        assert!(zip_after_seek.as_slice() == &whole_zip[whole_zip.len() - zip_after_seek.len()..])
    }
}
