use std::future::Future;
use std::io::{Result, SeekFrom};
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use assert2::assert;
use packed_struct::{PackedStruct, PackedStructSlice};
use pin_project::pin_project;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncSeek, ReadBuf};

use crate::crc_reader::CrcReader;
use crate::entry_data::{EntryData, EntryReader as _};
use crate::structs;
use crate::structs::PackedStructZippityExt as _;

/// Minimum version needed to extract the zip64 extensions required by zippity
pub const ZIP64_VERSION_TO_EXTRACT: u8 = 45;
pub(crate) const READ_SIZE: usize = 8192;

/// Represents an entry inside the [`Reader`].
#[derive(Clone, Debug)]
pub(crate) struct ReaderEntry<D: EntryData> {
    name: String,
    data: D,
    /// Offset of the entry's local header (first mention in the output) in the file
    offset: u64,
    crc32: Option<u32>,
    datetime: structs::DosDatetime,
    external_attributes: u32,
    /// Number of bytes of the entry data, cached from [`EntryData::size()`]
    size: u64,
}

impl<D: EntryData> ReaderEntry<D> {
    pub(crate) fn new(
        name: String,
        data: D,
        offset: u64,
        crc32: Option<u32>,
        datetime: structs::DosDatetime,
        external_attributes: u32,
        size: u64,
    ) -> Self {
        Self {
            name,
            data,
            offset,
            crc32,
            datetime,
            external_attributes,
            size,
        }
    }

    #[cfg(test)]
    pub(crate) fn get_name(&self) -> &str {
        self.name.as_str()
    }

    /// Obtains a [`EntryData::Reader`] for the entry wrapped in [`CrcReader`], keeping state in the given [`ReaderPinned`].
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

        if self.size == 0 {
            let crc = crc32fast::hash(&[]);
            self.crc32 = Some(crc);
            return Poll::Ready(Ok(crc));
        }

        let mut file_reader = ready!(self.get_reader(pinned.as_mut(), ctx))?;

        let mut read_buffer = get_read_buf(read_buffer);

        loop {
            read_buffer.clear();
            assert!(read_buffer.filled().is_empty());
            ready!(
                file_reader
                    .as_mut()
                    .poll_read(&self.data, ctx, &mut read_buffer)
            )?;
            if read_buffer.filled().is_empty() {
                break;
            }
        }

        let crc32 = file_reader
            .get_crc32()
            .expect("We didn't seek in the reader, so the CRC should be correctly calculated");

        pinned.set(ReaderPinned::Nothing);

        self.crc32 = Some(crc32);

        Poll::Ready(Ok(crc32))
    }
}

/// Describes a block of data inside the ZIP file.
/// This is either a structure of the ZIP format or entry data.
/// Serves as a high level "cursor" inside the file, with an offset within the chunk kept separately.
#[derive(Debug, Default)]
pub(crate) enum Chunk {
    LocalHeader {
        entry_index: usize,
    },
    EntryData {
        entry_index: usize,
    },
    DataDescriptor {
        entry_index: usize,
    },
    CDFileHeader {
        entry_index: usize,
    },
    Eocd,
    #[default]
    Finished,
}

impl Chunk {
    /// Initializes a new chunk descriptor to either point to the first local header,
    /// or to EOCD, if this is an empty ZIP.
    fn new<D: EntryData>(entries: &[ReaderEntry<D>]) -> Chunk {
        if entries.is_empty() {
            Chunk::Eocd
        } else {
            Chunk::LocalHeader { entry_index: 0 }
        }
    }

    /// Returns size of the current chunk in bytes.
    pub(crate) fn size<D: EntryData>(&self, entries: &[ReaderEntry<D>]) -> u64 {
        match self {
            Chunk::LocalHeader { entry_index } => {
                structs::LocalFileHeader::packed_size()
                    + entries[*entry_index].name.len() as u64
                    + structs::Zip64ExtraField::packed_size()
            }
            Chunk::EntryData { entry_index } => entries[*entry_index].size,
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

    /// Returns a chunk following `self` in the ZIP.
    pub(crate) fn next<D: EntryData>(&self, entries: &[ReaderEntry<D>]) -> Chunk {
        match self {
            Chunk::LocalHeader { entry_index } => Chunk::EntryData {
                entry_index: *entry_index,
            },
            Chunk::EntryData { entry_index } => Chunk::DataDescriptor {
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

/// Parts of the Reader state that need to be pinned.
/// This is a workaround to avoid multiple mutable borrows.
/// The variant represents a Future that we're waiting on currently.
#[pin_project(project = ReaderPinnedProj)]
enum ReaderPinned<D: EntryData> {
    Nothing,
    ReaderFuture(#[pin] D::Future),
    FileReader(#[pin] CrcReader<D::Reader>),
}

/// Parts of the state of reader that don't need pinning.
/// As a result, these can be accessed using a mutable reference
/// and can have mutable methods.
#[derive(Debug, Default)]
struct ReadState {
    /// Which chunk we are currently reading
    current_chunk: Chunk,
    /// How many bytes did we already read from the current chunk.
    /// Data in staging buffer counts as already processed.
    /// Used only for verification and for error reporting on [`EntryData`].
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

/// Main entry point for reading a zip file.
///
/// This struct allows asynchronous reading and seeking within a zip archive,
/// abstracting over the underlying data source (`D`).
///
/// To construct a reader, use [`crate::Builder`].
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

/// Represents an error that can occur during ZIP file reading or seeking.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ReadError {
    /// Entry reports (in [`EntryData::size()`]) a size that does not match the actual size.
    ///
    /// This error is not guaranteed to be detected when seeking in the ZIP.
    #[error("Entry {entry_name} reports size {expected_size} B, but was {actual_size} B")]
    SizeMismatch {
        /// The name of the entry.
        entry_name: String,
        /// The expected size of the entry.
        expected_size: u64,
        /// The actual size of the entry.
        actual_size: u64,
    },
    /// Entry was given a CRC value (in [`crate::BuilderEntry::crc32()`]) that does not match the computed one.
    ///
    /// This error is not guaranteed to be detected when seeking in the ZIP.
    #[error(
        "Entry {entry_name} was given a CRC value {expected_crc:08x} that does not match the computed {actual_crc:08x}"
    )]
    Crc32Mismatch {
        /// The name of the entry.
        entry_name: String,
        /// The expected CRC32 value.
        expected_crc: u32,
        /// The actual CRC32 value.
        actual_crc: u32,
    },
    /// Attempting to seek before the start of the file.
    #[error("Attempting to seek before the start of the file")]
    SeekingBeforeStart,
}

impl From<ReadError> for std::io::Error {
    fn from(value: ReadError) -> Self {
        std::io::Error::other(value)
    }
}

impl<D: EntryData> Reader<D> {
    pub(crate) fn new(sizes: Sizes, entries: Vec<ReaderEntry<D>>) -> Self {
        let read_state = ReadState::new(&entries);
        Reader {
            sizes,
            entries,
            read_state,
            pinned: ReaderPinned::Nothing,
        }
    }

    /// Creates a reader that reads an empty (but valid) ZIP file.
    /// The resulting Reader is useful mostly as a cheap dummy value for replacing an actual reader.
    #[must_use]
    pub fn new_empty_file() -> Self {
        let entries = Vec::new();
        let current_chunk = Chunk::new(&entries);
        Reader {
            sizes: Sizes {
                cd_offset: 0,
                cd_size: 0,
                eocd_offset: 0,
                total_size: crate::builder::eocd_size(),
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

    /// Takes the internal state of the pinned Reader, leaving state equivalent to
    /// the result of [`Reader::new_empty_file()`] (empty but valid ZIP).
    /// Returns a new Reader with state equivalent to `self` before the operation.
    /// This operation is O(1) and does not allocate.
    pub fn take_pinned(self: Pin<&mut Self>) -> Reader<D> {
        use std::mem::replace;

        let empty = Self::new_empty_file();
        let read_pos = self.tell();

        let mut projected = self.project();

        let sizes = replace(projected.sizes, empty.sizes);
        let entries = replace(projected.entries, empty.entries);
        let read_state = {
            let _ = replace(projected.read_state, empty.read_state);
            let mut read_state = ReadState::new(&entries);
            read_state.seek_from_start(&sizes, &entries, read_pos);
            read_state
        };
        projected.pinned.set(empty.pinned);
        let pinned = ReaderPinned::Nothing;

        Reader {
            sizes,
            entries,
            read_state,
            pinned,
        }
    }

    #[cfg(test)]
    pub(crate) fn get_entries(&self) -> &[ReaderEntry<D>] {
        &self.entries
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Sizes {
    pub(crate) cd_offset: u64,
    pub(crate) cd_size: u64,
    pub(crate) eocd_offset: u64,
    pub(crate) total_size: u64,
}

impl ReadState {
    fn new<D: EntryData>(entries: &[ReaderEntry<D>]) -> Self {
        Self {
            current_chunk: Chunk::new(entries),
            ..Default::default()
        }
    }

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
                    language_encoding: true,
                },
                compression: structs::Compression::Store,
                last_mod_datetime: entry.datetime,
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

    fn read_entry_data<D: EntryData>(
        &mut self,
        entry: &mut ReaderEntry<D>,
        mut pinned: Pin<&mut ReaderPinned<D>>,
        ctx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<Result<bool>> {
        let mut file_reader = ready!(entry.get_reader(pinned.as_mut(), ctx))?;

        if self.to_skip > 0 {
            // If there is some part of the file to skip, we have to decide whether to
            // seek to the new position, or to read the file whole.
            // As a performance optimization, we choose to read the data if the CRC
            // is not yet known (could have been set from cached values during construction),
            // to avoid re-opening the file and re-reading data when the CRC is requested in
            // data descriptor.
            // TODO: Measure that the complexity is worth it (but how?).
            if entry.crc32.is_none() {
                let mut read_buffer = get_read_buf(&mut self.staging_buffer);

                while self.to_skip > 0 {
                    read_buffer.clear();

                    // Limit the read size so that we don't go over the amount to skip
                    let limit = self.to_skip.try_into().unwrap_or(usize::MAX);
                    let mut read_buffer = read_buffer.take(limit);

                    assert!(read_buffer.filled().is_empty());
                    ready!(
                        file_reader
                            .as_mut()
                            .poll_read(&entry.data, ctx, &mut read_buffer)
                    )?;

                    self.chunk_processed_size += read_buffer.filled().len() as u64;
                    self.to_skip -= read_buffer.filled().len() as u64;

                    if read_buffer.filled().is_empty() {
                        // This means that we've skipped over the whole file.
                        // -> the reported size was wrong (and we will return an error later).
                        break;
                    }
                }
            } else {
                let pos_after_seek = ready!(file_reader.as_mut().seek(
                    &entry.data,
                    ctx,
                    SeekFrom::Start(self.to_skip)
                ))?;
                assert!(pos_after_seek == self.to_skip);
                self.chunk_processed_size += self.to_skip;
                self.to_skip = 0;
            }
        }

        let remaining_before_poll = output.remaining();
        ready!(file_reader.as_mut().poll_read(&entry.data, ctx, output))?;

        if output.remaining() == remaining_before_poll {
            // Nothing was output => we read everything in the file already

            if let Some(actual_crc) = file_reader.get_crc32() {
                if let Some(expected_crc) = entry.crc32
                    && expected_crc != actual_crc
                {
                    return Poll::Ready(Err(ReadError::Crc32Mismatch {
                        entry_name: entry.name.clone(),
                        expected_crc,
                        actual_crc,
                    }
                    .into()));
                }
                entry.crc32 = Some(actual_crc);
            }

            pinned.set(ReaderPinned::Nothing);

            if self.chunk_processed_size == entry.size {
                Poll::Ready(Ok(true)) // We're done with this state
            } else {
                Poll::Ready(Err(ReadError::SizeMismatch {
                    entry_name: entry.name.clone(),
                    expected_size: entry.size,
                    actual_size: self.chunk_processed_size,
                }
                .into()))
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
                compressed_size: entry.size,
                uncompressed_size: entry.size,
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
                    spec_version: ZIP64_VERSION_TO_EXTRACT,
                    os: structs::VersionMadeByOs::Unix,
                },
                version_to_extract: ZIP64_VERSION_TO_EXTRACT.into(),
                flags: structs::GpBitFlag {
                    use_data_descriptor: false,
                    language_encoding: true,
                },
                compression: structs::Compression::Store,
                last_mod_datetime: entry.datetime,
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
                disk_number_start: 0,
                internal_attributes: 0,
                external_attributes: entry.external_attributes,
                local_header_offset: u32::MAX,
            },
            output,
        );
        self.read_str(&entry.name, output);
        self.read_packed_struct(
            structs::Zip64ExtraField {
                tag: structs::Zip64ExtraField::TAG,
                size: structs::Zip64ExtraField::packed_size_u16() - 4,
                uncompressed_size: entry.size,
                compressed_size: entry.size,
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

        macro_rules! zippity_ready {
            ($expression:expr) => {
                match $expression {
                    Poll::Ready(Ok(x)) => x,
                    Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                    Poll::Pending if output.remaining() == initial_remaining => {
                        return Poll::Pending
                    }
                    _ => return Poll::Ready(Ok(())),
                }
            };
        }

        while output.remaining() > 0 {
            if !self.staging_buffer.is_empty() {
                self.read_from_staging(output);
                continue;
            }

            if let Chunk::Finished = self.current_chunk {
                break;
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
                Chunk::EntryData { entry_index } => {
                    let entry_index = *entry_index;
                    if output.remaining() != initial_remaining {
                        // We have already written something into the output -> interrupt this call, because
                        // we might need to return Pending when reading the file data
                        assert!(self.staging_buffer.is_empty());
                        break;
                    }
                    zippity_ready!(self.read_entry_data(
                        &mut entries[entry_index],
                        pinned.as_mut(),
                        ctx,
                        output,
                    ))
                }
                Chunk::DataDescriptor { entry_index } => {
                    let entry_index = *entry_index;
                    zippity_ready!(self.read_data_descriptor(
                        &mut entries[entry_index],
                        pinned.as_mut(),
                        ctx,
                        output
                    ))
                }
                Chunk::CDFileHeader { entry_index } => {
                    let entry_index = *entry_index;
                    zippity_ready!(self.read_cd_file_header(
                        &mut entries[entry_index],
                        pinned.as_mut(),
                        ctx,
                        output
                    ))
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

    /// Seeks to a given offset inside the file.
    /// After seeking using this method, the corresponding `pinned` field has to be cleared too.
    /// Always succeeds, seeking past end of file causes `tell()` to return the
    /// set position and reads returning empty buffers (=EOF).
    fn seek_from_start<D: EntryData>(
        &mut self,
        sizes: &Sizes,
        entries: &[ReaderEntry<D>],
        offset: u64,
    ) {
        let (chunk, chunk_position) = if offset >= sizes.eocd_offset {
            (Chunk::Eocd, sizes.eocd_offset)
        } else if offset >= sizes.cd_offset {
            assert!(!entries.is_empty());
            (Chunk::CDFileHeader { entry_index: 0 }, sizes.cd_offset)
        } else {
            assert!(!entries.is_empty());
            let entry_index = match entries.binary_search_by_key(&offset, |entry| entry.offset) {
                Ok(index) => index,
                Err(index) => index - 1,
            };
            assert!(entry_index < entries.len());
            assert!(entries[entry_index].offset <= offset);
            assert!(entry_index == entries.len() - 1 || entries[entry_index + 1].offset > offset);
            (
                Chunk::LocalHeader { entry_index },
                entries[entry_index].offset,
            )
        };

        self.current_chunk = chunk;
        self.chunk_processed_size = 0;
        self.position = offset;
        self.staging_buffer.clear();
        self.to_skip = offset - chunk_position;
    }
}

impl<D: EntryData> Reader<D> {
    /// Return the total size of the file in bytes.
    pub fn size(&self) -> u64 {
        self.sizes.total_size
    }

    /// Seeks to given offset from start of zip file.
    ///
    /// This version allows seeking before the Reader is pinned.
    ///
    /// Always succeeds, seeking past end of file causes `tell()` to return the
    /// set offset and reads returning empty buffers (=EOF).
    pub fn seek_from_start_mut(&mut self, offset: u64) {
        self.read_state
            .seek_from_start(&self.sizes, self.entries.as_slice(), offset);
        self.pinned = ReaderPinned::Nothing;
    }

    /// Seeks to given offset from start of zip file.
    ///
    /// This is the version for pinned Reader.
    ///
    /// Always succeeds, seeking past end of file causes `tell()` to return the
    /// set offset and reads returning empty buffers (=EOF).
    pub fn seek_from_start_pinned(self: Pin<&mut Self>, offset: u64) {
        let mut projected = self.project();
        projected
            .read_state
            .seek_from_start(projected.sizes, projected.entries.as_slice(), offset);
        projected.pinned.set(ReaderPinned::Nothing);
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
                Ok(result)
            } else if offset < 0 {
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    ReadError::SeekingBeforeStart,
                ))
            } else {
                core::panic!("Attempting to seek to a position that causes u64 overflow");
            }
        };
        let pos = match position {
            SeekFrom::Start(pos) => pos,
            SeekFrom::Current(offset) => resolve_offset(self.tell(), offset)?,
            SeekFrom::End(offset) => resolve_offset(self.size(), offset)?,
        };

        self.seek_from_start_pinned(pos);
        Ok(())
    }

    fn poll_complete(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<u64>> {
        Poll::Ready(Ok(self.tell()))
    }
}

impl<D: EntryData + std::fmt::Debug> std::fmt::Debug for Reader<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("zippity::Reader").finish_non_exhaustive()
    }
}

impl<D: EntryData + Clone> Clone for Reader<D> {
    fn clone(&self) -> Self {
        let mut cloned = Reader::new(self.sizes.clone(), self.entries.clone());
        cloned.seek_from_start_mut(self.tell());
        cloned
    }
}

fn get_read_buf(vec: &mut Vec<u8>) -> ReadBuf<'_> {
    vec.reserve(READ_SIZE);
    ReadBuf::uninit(&mut vec.spare_capacity_mut()[..READ_SIZE])
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::Builder;
    use crate::test_util::{
        funky_entry_data, measure_size, read_size_strategy, read_to_vec, skip_length,
        test_entry_data::{ArbitraryReaderParams, TestEntryData},
    };
    use assert2::assert;
    use std::{io::ErrorKind, pin::pin};
    use test_strategy::proptest;
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    use packed_struct::prelude::*;
    #[derive(Debug, PackedStruct)]
    #[packed_struct(endian = "msb")]
    struct TestPs {
        v: u64,
    }

    impl TestPs {
        fn new() -> TestPs {
            TestPs {
                v: 0x0102_0304_0506_0708,
            }
        }
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

        let mut buf_backing = vec![0; initial_output_content.len() + output_buffer_extra_size + 8];
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

        let mut buf_backing = vec![0; initial_output_content.len() + output_buffer_extra_size];
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

        let mut buf_backing = vec![0; initial_output_content.len() + output_buffer_extra_size + 1]; // Always have room for at least 1 byte to read
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
        let to_skip = skip_length(initial_buffer_content.len(), to_skip_f);
        let mut rs = ReadState {
            current_chunk: Chunk::Finished,
            chunk_processed_size: 0,
            staging_buffer: initial_buffer_content.clone(),
            to_skip,
            position: 0,
        };

        let mut buf_backing = vec![0; initial_output_content.len() + output_buffer_extra_size + 1]; // Always have room for at least 1 byte to read
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

        let mut buf_backing =
            vec![0; initial_output_content.len() + output_buffer_extra_size + test_data.len()];
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

        let mut buf_backing = vec![0; initial_output_content.len() + output_buffer_extra_size];
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
        let to_skip = 1 + skip_length(test_data.len() - 2, to_skip_f);
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

        let mut buf_backing = vec![0; initial_output_content.len() + output_buffer_extra_size + 1]; // Always have room for at least 1 byte to read
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
        let to_skip = skip_length(initial_buffer_content.len(), to_skip_f);
        let mut rs = ReadState {
            current_chunk: Chunk::Finished,
            chunk_processed_size: 0,
            staging_buffer: initial_buffer_content.clone(),
            to_skip,
            position: 0,
        };

        let mut buf_backing = vec![0; initial_output_content.len() + output_buffer_extra_size + 1]; // Always have room for at least 1 byte to read
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
        #[strategy(1usize..100usize)] to_skip: usize,
        #[strategy(1usize..100usize)] read_size: usize,
    ) {
        // Calculate range in test data that will be added to the output.
        let start = to_skip;
        let stop = (start + read_size).min(test_data.len());
        let start = start.min(stop);

        let to_skip = to_skip as u64;
        let mut rs = ReadState {
            current_chunk: Chunk::Finished,
            chunk_processed_size: 0,
            staging_buffer: test_data.clone(),
            to_skip,
            position: 0,
        };

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
            assert!(
                &rs.staging_buffer[usize::try_from(rs.to_skip).unwrap()..] == &test_data[stop..],
                "What remains in the staging buffer after the skip must be what we didn't write from the test data"
            );
        }
    }

    /// Tests that a zip archive with a large file (> 4GB) can be created.
    ///
    /// Doesn't check that it unpacks correctly to keep the requirements low,
    /// only verifies that the expected and actual sizes match.
    /// This doesn't actually store the file or archive, but is still fairly slow.
    #[tokio::test]
    async fn zip64_size() {
        let mut builder = Builder::<funky_entry_data::Zeros>::new();
        builder
            .add_entry(
                "Big file".to_owned(),
                funky_entry_data::Zeros::new(0x1_0000_0001),
            )
            .unwrap();
        let zippity = pin!(builder.build());

        let expected_size = zippity.size();
        let actual_size = measure_size(zippity).await.unwrap();

        assert!(actual_size == expected_size);
    }

    /// Tests that a zip archive with many files (> 65536) can be created.
    ///
    /// Doesn't check that it unpacks correctly to keep the requirements low,
    /// only verifies that the expected and actual sizes match.
    #[tokio::test]
    async fn zip64_count() {
        let mut builder = Builder::<&[u8]>::new();
        for i in 0..0x10001 {
            builder
                .add_entry(format!("Empty file {i}"), b"".as_ref())
                .unwrap();
        }
        let zippity = pin!(builder.build());

        let expected_size = zippity.size();
        let actual_size = measure_size(zippity).await.unwrap();

        assert!(actual_size == expected_size);
    }

    /// Tests that passing a LazyReader (that delays each async operation) ends up with the same
    /// archive as when using regular slice based input.
    #[proptest(async = "tokio")]
    async fn lazy_reader_equal_result(
        content: TestEntryData,
        #[strategy(read_size_strategy())] read_size: usize,
    ) {
        let mut builder_slice = Builder::<&[u8]>::new();
        let mut builder_lazy = Builder::<funky_entry_data::LazyReader>::new();

        for (name, value) in &content.0 {
            builder_slice
                .add_entry(name.clone(), value.as_ref())
                .unwrap();
            builder_lazy
                .add_entry(name.clone(), value.as_ref())
                .unwrap();
        }

        let zippity_slice = pin!(builder_slice.build());
        let output_slice = read_to_vec(zippity_slice, read_size).await.unwrap();
        let zippity_lazy = pin!(builder_lazy.build());
        let output_lazy = read_to_vec(zippity_lazy, read_size).await.unwrap();

        assert!(output_slice == output_lazy);
    }

    /// Prepare a reader with data from a hash map and a vector of the zip
    /// read as a whole
    async fn prepare_seek_test_data(
        content: TestEntryData,
        read_size: usize,
    ) -> (Reader<Vec<u8>>, Vec<u8>) {
        let builder = Builder::from(content);

        let zippity_whole = pin!(builder.clone().build());
        let buf_whole = read_to_vec(zippity_whole, read_size).await.unwrap();

        let zippity_ret = builder.build();

        (zippity_ret, buf_whole)
    }

    /// Seek the reader to a given position and verify that the remaining data
    /// and returned position matches the end of the given ground truth data.
    async fn seek_read_and_verify<T: EntryData>(
        reader: Reader<T>,
        read_size: usize,
        seek_from: SeekFrom,
        ground_truth: &[u8],
    ) -> u64 {
        let mut reader = pin!(reader);

        let seek_reported = reader.seek(seek_from).await.unwrap();
        let buf = read_to_vec(reader, read_size).await.unwrap();

        assert!(buf.as_slice() == &ground_truth[usize::try_from(seek_reported).unwrap()..]);

        seek_reported
    }

    /// Test that seeking to a valid location in a zip file using SeekFrom::Start works as expected
    #[proptest(async = "tokio")]
    async fn seeking_from_start(
        content: TestEntryData,
        #[strategy(read_size_strategy())] read_size: usize,
        #[strategy(0f64..=1f64)] seek_pos_fraction: f64,
    ) {
        let (reader, buf_whole) = prepare_seek_test_data(content, read_size).await;
        let seek_pos = skip_length(buf_whole.len(), seek_pos_fraction);
        let reported_pos = seek_read_and_verify(
            reader,
            read_size,
            SeekFrom::Start(seek_pos),
            buf_whole.as_slice(),
        )
        .await;
        assert!(reported_pos == seek_pos);
    }

    /// Test that seeking to a valid location in a zip file using SeekFrom::End works as expected
    #[proptest(async = "tokio")]
    async fn seeking_from_end(
        content: TestEntryData,
        #[strategy(read_size_strategy())] read_size: usize,
        #[strategy(0f64..=1f64)] seek_pos_fraction: f64,
    ) {
        let (reader, buf_whole) = prepare_seek_test_data(content, read_size).await;
        let seek_pos = skip_length(buf_whole.len(), seek_pos_fraction);
        let reported_pos = seek_read_and_verify(
            reader,
            read_size,
            SeekFrom::End(
                i64::try_from(seek_pos).unwrap() - i64::try_from(buf_whole.len()).unwrap(),
            ),
            buf_whole.as_slice(),
        )
        .await;
        assert!(reported_pos == seek_pos);
    }

    /// Test that seeking to a valid location in a zip file using SeekFrom::Current works as expected
    /// Does an extra seek first to move the cursor
    #[proptest(async = "tokio")]
    async fn seeking_from_cursor(
        content: TestEntryData,
        #[strategy(read_size_strategy())] read_size: usize,
        #[strategy(0f64..=1f64)] seek_pos1_fraction: f64,
        #[strategy(0f64..=1f64)] seek_pos2_fraction: f64,
    ) {
        let (mut reader, buf_whole) = prepare_seek_test_data(content, read_size).await;
        let seek_pos1 = skip_length(buf_whole.len(), seek_pos1_fraction);
        let seek_pos2 = skip_length(buf_whole.len(), seek_pos2_fraction);
        reader.seek(SeekFrom::Start(seek_pos1)).await.unwrap();

        let reported_pos = seek_read_and_verify(
            reader,
            read_size,
            SeekFrom::Current(
                i64::try_from(seek_pos2).unwrap() - i64::try_from(seek_pos1).unwrap(),
            ),
            buf_whole.as_slice(),
        )
        .await;
        assert!(reported_pos == seek_pos2);
    }

    /// Test that reading multiple single bytes inside the zip file with absolute seeks
    /// in between returns the same bytes as reading the whole file.
    /// Also tests reusing the zippity reader object.
    #[proptest(async = "tokio")]
    async fn seeking_single_bytes(
        content: TestEntryData,
        #[strategy(proptest::collection::vec(0f64..=1f64, 1..100))] byte_positions_f: Vec<f64>,
    ) {
        let (mut reader, buf_whole) = prepare_seek_test_data(content, 8192).await;
        for fraction in byte_positions_f {
            let seek_pos = skip_length(buf_whole.len(), fraction);

            let expected = buf_whole[usize::try_from(seek_pos).unwrap()];

            reader.seek(SeekFrom::Start(seek_pos)).await.unwrap();
            let actual = reader.read_u8().await.unwrap();
            assert!(actual == expected);
        }
    }

    /// Test that seeking to a negative location results in an error.
    #[proptest(async = "tokio")]
    async fn seeking_before_start(distance: u8) {
        let mut reader = pin!(Builder::<&[u8]>::new().build());
        let seek_offset = -i64::from(distance) - 1;
        let err = reader
            .seek(SeekFrom::Current(seek_offset))
            .await
            .unwrap_err();
        dbg!(&err);
        assert!(err.kind() == ErrorKind::InvalidInput);
        let message = format!("{}", err.into_inner().unwrap());

        assert!(message.contains("before"));
    }

    /// Test that seeking to a valid location in a zip file using SeekFrom::Start works as expected
    #[proptest(async = "tokio")]
    async fn seeking_after_end_from_start(distance: u8) {
        let mut reader = pin!(Builder::<&[u8]>::new().build());
        let seek_pos = reader.size() + u64::from(distance);
        let reported_position = reader.seek(SeekFrom::Start(seek_pos)).await.unwrap();
        assert!(reported_position == seek_pos);

        let remaining_content = read_to_vec(reader, 16).await.unwrap();
        assert!(remaining_content.is_empty());
    }

    /// Test that seeking to a valid location in a zip file using SeekFrom::Start works as expected
    #[proptest(async = "tokio")]
    async fn seeking_after_end_from_current(distance: u8) {
        let mut reader = pin!(Builder::<&[u8]>::new().build());
        let seek_position = i64::try_from(reader.size()).unwrap() + i64::from(distance);
        let reported_position = reader.seek(SeekFrom::Current(seek_position)).await.unwrap();
        assert!(reported_position == u64::try_from(seek_position).unwrap());

        let remaining_content = read_to_vec(reader, 16).await.unwrap();
        assert!(remaining_content.is_empty());
    }

    /// Test that seeking to a valid location in a zip file using SeekFrom::Start works as expected
    #[proptest(async = "tokio")]
    async fn tell(
        content: TestEntryData,
        #[strategy(1usize..100usize)] read_size: usize,
        #[strategy(0f64..=1f64)] seek_pos_fraction: f64,
    ) {
        let (mut reader, buf_whole) = prepare_seek_test_data(content, read_size).await;
        dbg!(buf_whole.len());
        assert!(reader.tell() == 0);
        let seek_pos = skip_length(buf_whole.len(), seek_pos_fraction);
        reader.seek(SeekFrom::Start(seek_pos)).await.unwrap();
        assert!(reader.tell() == seek_pos);
        dbg!(seek_pos);

        let mut buffer = vec![0; read_size];
        let bytes_read = reader.read(buffer.as_mut_slice()).await.unwrap();
        dbg!(bytes_read);

        assert!(reader.tell() == seek_pos + bytes_read as u64);
    }

    #[tokio::test]
    async fn bad_size_entry_data_errors_out() {
        let mut builder: Builder<funky_entry_data::BadSize> = Builder::new();
        builder
            .add_entry(
                "xxx".into(),
                funky_entry_data::BadSize {
                    reported_size: 100,
                    actual_size: 10,
                },
            )
            .unwrap();

        let zippity = pin!(builder.build());
        let e = read_to_vec(zippity, 1024).await.unwrap_err();

        assert!(e.kind() == ErrorKind::Other);
        let message = format!("{}", e.into_inner().unwrap());

        assert!(message.contains("xxx"));
    }

    mod bad_size_seek {
        use super::*;

        async fn test_internal(reported_size: u64, actual_size: u64, seek_pos_in_entry: u64) {
            let mut builder: Builder<funky_entry_data::BadSize> = Builder::new();
            builder
                .add_entry(
                    "xxx".into(),
                    funky_entry_data::BadSize {
                        reported_size,
                        actual_size,
                    },
                )
                .unwrap();

            let mut zippity = pin!(builder.build());
            zippity
                .seek(SeekFrom::Start(
                    seek_pos_in_entry
                        + Chunk::LocalHeader { entry_index: 0 }.size(zippity.entries.as_slice()),
                ))
                .await
                .unwrap();
            let e = read_to_vec(zippity, 1024).await.unwrap_err();
            dbg!(e);
        }

        #[tokio::test]
        async fn reported_too_short_before_reported_size() {
            test_internal(10, 20, 5).await;
        }

        #[tokio::test]
        #[should_panic(expected = "called `Result::unwrap_err()` on an `Ok` value")] //TODO: This kind of bad input can't be detected at the moment.
        async fn reported_too_short_after_reported_size() {
            test_internal(10, 20, 15).await;
        }

        #[tokio::test]
        async fn reported_too_long_before_real_size() {
            test_internal(20, 10, 5).await;
        }

        #[tokio::test]
        async fn reported_too_long_after_real_size() {
            test_internal(20, 10, 15).await;
        }
    }

    mod crc_recalculation_with_seek {
        use super::*;
        use assert2::assert;

        /// Gray box test that verifies behavior of CRC recalculation.
        /// Either seeks within file data (possibility to recalculate CRC when
        /// producing remaining data), or after the file data (has to be recalculated separately).
        /// Either pre-fills the CRC from ground truth run or not.
        /// In all cases compares the remainder of the seek with ground truth
        /// generated without seeking and without pre-filled CRC.
        ///
        /// Since this is a gray box test, it is potentially fragile wrt changes
        /// to the `Chunk` design of the reader.
        async fn test_internal(test_data: &[u8], pre_fill_crc: bool, seek_past_data: bool) {
            let mut builder1 = Builder::<funky_entry_data::LazyReader>::new();
            builder1.add_entry("X".to_owned(), test_data).unwrap();

            let mut reader1 = pin!(builder1.build());
            let whole_zip = read_to_vec(reader1.as_mut(), 8192).await.unwrap();

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

            let mut builder2 = Builder::<funky_entry_data::LazyReader>::new();
            let entry = builder2.add_entry("X".to_owned(), test_data).unwrap();
            if pre_fill_crc {
                entry.crc32(entry_crc);
            }
            let mut reader2 = pin!(builder2.build());

            if seek_past_data {
                reader2
                    .as_mut()
                    .seek(SeekFrom::Start(
                        test_data.len() as u64
                            + Chunk::LocalHeader { entry_index: 0 }
                                .size(reader1.entries.as_slice()),
                    ))
                    .await
                    .unwrap();
            } else {
                reader2
                    .as_mut()
                    .seek(SeekFrom::Start(test_data.len() as u64))
                    .await
                    .unwrap();
            }

            let zip_after_seek = read_to_vec(reader2.as_mut(), 8192).await.unwrap();

            assert!(
                zip_after_seek.as_slice() == &whole_zip[whole_zip.len() - zip_after_seek.len()..]
            );
        }

        #[proptest(async = "tokio")]
        async fn cached_crc_seek_within(test_data: Vec<u8>) {
            test_internal(test_data.as_slice(), true, false).await;
        }

        #[proptest(async = "tokio")]
        async fn cached_crc_seek_past(test_data: Vec<u8>) {
            test_internal(test_data.as_slice(), true, true).await;
        }

        #[proptest(async = "tokio")]
        async fn no_cached_crc_seek_within(test_data: Vec<u8>) {
            test_internal(test_data.as_slice(), false, false).await;
        }

        #[proptest(async = "tokio")]
        async fn no_cached_crc_seek_past(test_data: Vec<u8>) {
            test_internal(test_data.as_slice(), false, true).await;
        }
    }

    #[proptest(async = "tokio")]
    async fn empty_data_dont_open_reader(#[strategy(read_size_strategy())] read_size: usize) {
        let mut builder = Builder::<funky_entry_data::EmptyUnsupportedReader>::new();
        builder
            .add_entry("abc".into(), funky_entry_data::EmptyUnsupportedReader())
            .unwrap();
        builder
            .add_entry("def".into(), funky_entry_data::EmptyUnsupportedReader())
            .unwrap();
        builder
            .add_entry("ghi".into(), funky_entry_data::EmptyUnsupportedReader())
            .unwrap();

        let zippity = pin!(builder.build());

        read_to_vec(zippity, read_size).await.unwrap();
        // No need to check anything else, get_reader is already unimplemented
    }

    #[tokio::test]
    async fn bad_crc() {
        let mut builder = Builder::<&[u8]>::new();
        builder
            .add_entry("x".into(), b"x".as_slice())
            .unwrap()
            .crc32(0); // Passing in a wrong CRC

        let zippity = pin!(builder.build());
        let e = read_to_vec(zippity, 8192).await.unwrap_err();
        dbg!(e);
    }

    /// Test that seeking to a valid location in a zip file using SeekFrom::Start works as expected
    #[proptest(async = "tokio")]
    async fn clone(
        mut reader: Reader<Vec<u8>>,
        #[strategy(read_size_strategy())] read_size: usize,
        #[strategy(0f64..=1f64)] seek_pos_fraction: f64,
    ) {
        let seek_pos = skip_length(usize::try_from(reader.size()).unwrap(), seek_pos_fraction);
        reader.seek_from_start_mut(seek_pos);

        let cloned = pin!(reader.clone());
        let reader = pin!(reader);

        let original_data = read_to_vec(reader, read_size).await.unwrap();
        let cloned_data = read_to_vec(cloned, read_size).await.unwrap();

        assert!(cloned_data == original_data);
    }

    mod new_empty {
        use super::*;
        use assert2::assert;

        /// Test that constructing an empty reader object works and provides no bytes.
        #[proptest(async = "tokio")]
        async fn read(#[strategy(read_size_strategy())] read_size: usize) {
            let zippity = pin!(Reader::<&[u8]>::new_empty_file());
            let expected_size = zippity.size();
            let v = read_to_vec(zippity, read_size).await.unwrap();

            assert!(v.len() as u64 == expected_size);
        }

        /// Test that empty constructed reader can be seeked in and stoll provides no data.
        #[proptest(async = "tokio")]
        async fn seek(
            #[strategy(read_size_strategy())] read_size: usize,
            #[strategy(0f64..=1f64)] seek_pos_fraction: f64,
        ) {
            let mut zippity = pin!(Reader::<&[u8]>::new_empty_file());
            let v = read_to_vec(zippity.as_mut(), read_size).await.unwrap();
            let seek_pos = skip_length(v.len(), seek_pos_fraction);

            zippity.as_mut().seek_from_start_pinned(seek_pos);
            let v_seeked = read_to_vec(zippity.as_mut(), read_size).await.unwrap();

            assert!(v_seeked == v[usize::try_from(seek_pos).unwrap()..])
        }

        /// Test that Reader::new_empty_file() provides the same result as using an empty Builder.
        #[tokio::test]
        async fn builder_equivalence() {
            let from_new = read_to_vec(pin!(Reader::<&[u8]>::new_empty_file()), 8192)
                .await
                .unwrap();
            let from_builder = read_to_vec(pin!(Builder::<&[u8]>::new().build()), 8192)
                .await
                .unwrap();

            assert!(from_new == from_builder);
        }
    }

    /// Test that taking a reader returns the original data.
    #[proptest(async = "tokio")]
    async fn take_pinned(
        #[any(ArbitraryReaderParams { seek: true, ..Default::default()})] reader: Reader<Vec<u8>>,
    ) {
        let size = reader.size();
        let pos = reader.tell();

        let mut reader = pin!(reader);
        let taken = pin!(reader.as_mut().take_pinned());

        let data = read_to_vec(taken, 8192).await.unwrap();
        assert!(data.len() as u64 + pos == size);
    }
}
