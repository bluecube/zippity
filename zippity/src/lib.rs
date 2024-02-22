use assert2::assert;
use crc_reader::CrcReader;
use lru::LruCache;
use packed_struct::{PackedStruct, PackedStructSlice};
use pin_project::pin_project;
use std::collections::BTreeMap;
use std::future::Future;
use std::io::{Error, Result, SeekFrom};
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use structs::PackedStructZippityExt;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncSeek, ReadBuf};

mod crc_reader;
mod structs;
mod test_util;

/// Minimum version needed to extract the zip64 extensions required by zippity
pub const ZIP64_VERSION_TO_EXTRACT: u16 = 45;

pub trait EntryData {
    type Reader: AsyncRead;
    type ReaderFuture: Future<Output = Result<Self::Reader>>;

    fn get_size(&self) -> u64;
    fn get_reader(&self) -> Self::ReaderFuture;
}

#[derive(Debug, Hash, Clone, PartialEq, Eq)]
struct CrcCacheKey {}

pub struct CrcCache(LruCache<CrcCacheKey, u32>);

impl CrcCache {
    pub fn new(limit: NonZeroUsize) -> Self {
        CrcCache(LruCache::new(limit))
    }

    pub fn unbounded() -> Self {
        CrcCache(LruCache::unbounded())
    }
}

impl EntryData for () {
    type Reader = std::io::Cursor<&'static [u8]>;
    type ReaderFuture = std::future::Ready<Result<Self::Reader>>;

    fn get_size(&self) -> u64 {
        0
    }

    fn get_reader(&self) -> Self::ReaderFuture {
        std::future::ready(Ok(std::io::Cursor::new(&[])))
    }
}

impl<'a> EntryData for &'a [u8] {
    type Reader = std::io::Cursor<&'a [u8]>;
    type ReaderFuture = std::future::Ready<Result<Self::Reader>>;

    fn get_size(&self) -> u64 {
        self.len() as u64
    }

    fn get_reader(&self) -> Self::ReaderFuture {
        std::future::ready(Ok(std::io::Cursor::new(self)))
    }
}

#[derive(Clone, Debug)]
struct BuilderEntry<D> {
    data: D,
}

impl<D: EntryData> BuilderEntry<D> {
    fn get_local_size(&self, name: &str) -> u64 {
        let local_header = structs::LocalFileHeader::packed_size();
        let zip64_extra_data = structs::Zip64ExtraField::packed_size();
        let filename = name.len() as u64;
        let data = self.data.get_size();
        let data_descriptor = structs::DataDescriptor64::packed_size();

        let size = local_header + zip64_extra_data + filename + data + data_descriptor;
        size
    }

    fn get_cd_header_size(&self, name: &str) -> u64 {
        let filename = name.len() as u64;
        let cd_entry = structs::CentralDirectoryHeader::packed_size();
        let zip64_extra_data = structs::Zip64ExtraField::packed_size();

        let size = cd_entry + zip64_extra_data + filename;
        size
    }
}

#[derive(Debug)]
struct ReaderEntry<D: EntryData> {
    name: String,
    data: D,
    offset: u64,
    crc32: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct Builder<D: EntryData> {
    entries: BTreeMap<String, BuilderEntry<D>>,
}

impl<D: EntryData> Builder<D> {
    pub fn new() -> Self {
        Builder {
            entries: BTreeMap::new(),
        }
    }

    pub fn add_entry<T: Into<D>>(&mut self, name: String, data: T) {
        let data = data.into();
        self.entries
            .insert(name, BuilderEntry { data: data.into() });
    }

    pub fn build(self) -> Reader<D> {
        // TODO: Allow filling CRCs from cache.
        let mut offset: u64 = 0;
        let mut cd_size: u64 = 0;
        let entries: Vec<_> = self
            .entries
            .into_iter()
            .map(|(name, entry)| {
                let size = entry.get_local_size(&name);
                let offset_copy = offset;
                offset += size;
                cd_size += entry.get_cd_header_size(&name);
                ReaderEntry {
                    name,
                    data: entry.data,
                    offset: offset_copy,
                    crc32: None,
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
                buffer: Vec::new(),
                to_skip: 0,
            },
            pinned: ReaderPinned::Nothing,
        }
    }
}

#[derive(Debug)]
enum Chunk {
    LocalHeader { entry_index: usize },
    FileData { entry_index: usize },
    DataDescriptor { entry_index: usize },
    CDFileHeader { entry_index: usize },
    EOCD,
    Finished,
}

impl Chunk {
    fn new<D: EntryData>(entries: &Vec<ReaderEntry<D>>) -> Chunk {
        if entries.is_empty() {
            Chunk::EOCD
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
            Chunk::FileData { entry_index } => entries[*entry_index].data.get_size(),
            Chunk::DataDescriptor { entry_index: _ } => structs::DataDescriptor64::packed_size(),
            Chunk::CDFileHeader { entry_index } => {
                structs::CentralDirectoryHeader::packed_size()
                    + entries[*entry_index].name.len() as u64
                    + structs::Zip64ExtraField::packed_size()
            }
            Chunk::EOCD => {
                structs::Zip64EndOfCentralDirectoryLocator::packed_size()
                    + structs::Zip64EndOfCentralDirectoryRecord::packed_size()
                    + structs::EndOfCentralDirectory::packed_size()
            }
            Chunk::Finished => 0,
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
                    Chunk::EOCD
                }
            }
            Chunk::EOCD => Chunk::Finished,
            Chunk::Finished => Chunk::Finished,
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
    chunk_processed_size: u64,
    /// Buffer for data that couldn't get written to the output directly.
    buffer: Vec<u8>,
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

struct Sizes {
    cd_offset: u64,
    cd_size: u64,
    eocd_offset: u64,
    total_size: u64,
}

impl ReadState {
    /// Read packed struct.
    /// Either reads to `output` (fast path), or to `self.buffer`
    /// Never writes to output, if buffer is non empty, or if we need to skip something
    fn read_packed_struct<P>(&mut self, ps: P, output: &mut ReadBuf<'_>)
    where
        P: PackedStruct,
    {
        let size = P::packed_size() as usize;
        self.chunk_processed_size += size as u64;

        if self.to_skip > size as u64 {
            self.to_skip -= size as u64;
        } else {
            if (self.to_skip == 0u64) & (output.remaining() >= size) & self.buffer.is_empty() {
                // Shortcut: Serialize directly to output
                let output_slice = output.initialize_unfilled_to(size);
                ps.pack_to_slice(output_slice).unwrap();
                output.advance(size);
            } else {
                // The general way: Pack to the buffer and it will get written some time later
                let buf_index = self.buffer.len();
                self.buffer.resize(buf_index + size, 0);
                ps.pack_to_slice(&mut self.buffer[buf_index..]).unwrap();
            }
        }
    }

    /// Read string slice to the output or to the buffer
    /// Never writes to output, if buffer is non empty, or if we need to skip something
    fn read_str(&mut self, s: &str, output: &mut ReadBuf<'_>) {
        let bytes = s.as_bytes();
        self.chunk_processed_size += bytes.len() as u64;

        if self.to_skip > bytes.len() as u64 {
            self.to_skip -= bytes.len() as u64;
        } else {
            if (self.to_skip == 0u64) & (output.remaining() >= bytes.len()) & self.buffer.is_empty()
            {
                output.put_slice(bytes);
            } else {
                // The general way: Pack to the buffer and it will get written some time later
                self.buffer.extend_from_slice(bytes);
            }
        }
    }

    /// Read from the buffer to output
    fn read_from_buffer(&mut self, output: &mut ReadBuf<'_>) {
        let start = self.to_skip as usize;
        let end = start + output.remaining();
        let last_read = end >= self.buffer.len();
        let end = if last_read { self.buffer.len() } else { end };

        if start < self.buffer.len() {
            // Most of the time (always in normal code?) the branch should be taken, because otherwise there's no
            // point in putting anything in the buffer in the first place.
            // Hovewer including it makes this function more robust and easier to test.
            output.put_slice(&self.buffer[start..end]);
        }

        if last_read {
            self.to_skip = 0;
            self.buffer.clear();
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
                version_to_extract: ZIP64_VERSION_TO_EXTRACT,
                flags: structs::GpBitFlag {
                    use_data_descriptor: true,
                },
                compression: structs::Compression::Store,
                last_mod_time: 0,
                last_mod_date: 0,
                crc32: 0,
                compressed_size: 0xffffffff,
                uncompressed_size: 0xffffffff,
                file_name_len: entry.name.len() as u16,
                extra_field_len: structs::Zip64ExtraField::packed_size() as u16,
            },
            output,
        );
        self.read_str(&entry.name, output);
        self.read_packed_struct(
            structs::Zip64ExtraField {
                tag: structs::Zip64ExtraField::TAG,
                size: structs::Zip64ExtraField::packed_size() as u16 - 4,
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
        let expected_size = entry.data.get_size();

        if let ReaderPinnedProj::Nothing = pinned.as_mut().project() {
            let reader_future = entry.data.get_reader();
            pinned.set(ReaderPinned::ReaderFuture(reader_future));
        }

        if let ReaderPinnedProj::ReaderFuture(ref mut reader_future) = pinned.as_mut().project() {
            let reader = ready!(reader_future.as_mut().poll(ctx))?;
            pinned.set(ReaderPinned::FileReader(CrcReader::new(reader)));
        }

        let ReaderPinnedProj::FileReader(ref mut file_reader) = pinned.as_mut().project() else {
            panic!("FileReader must be available at this point because of the preceding two conditions");
        };

        // TODO: We might want to decide to not recompute the CRC and seek instead
        while self.to_skip > 0 {
            // Construct a temporary output buffer in the unused part of the real output buffer,
            // but not large enough to read more than the ammount to skip
            // TODO: Wouldn't it be better to use the Vec that we keep anyway?
            let mut tmp_output = output.take(self.to_skip.try_into().unwrap_or(usize::MAX));
            assert!(tmp_output.filled().is_empty());

            ready!(file_reader.as_mut().poll_read(ctx, &mut tmp_output))?;

            self.chunk_processed_size += tmp_output.filled().len() as u64;
            self.to_skip -= tmp_output.filled().len() as u64;
        }

        let remaining_before_poll = output.remaining();
        ready!(file_reader.as_mut().poll_read(ctx, output))?;

        if output.remaining() == remaining_before_poll {
            // Nothing was output => we read everything in the file already

            entry.crc32 = Some(file_reader.get_crc32());

            pinned.set(ReaderPinned::Nothing);

            if self.chunk_processed_size == expected_size {
                Poll::Ready(Ok(true)) // We're done with this state
            } else {
                Poll::Ready(Err(Error::other(Box::new(ZippityError::LengthMismatch {
                    entry_name: entry.name.clone(),
                    expected_size,
                    actual_size: self.chunk_processed_size,
                }))))
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
        entry: &ReaderEntry<D>,
        output: &mut ReadBuf<'_>,
    ) -> bool {
        // TODO: Recalculate CRC if needed
        self.read_packed_struct(
            structs::DataDescriptor64 {
                signature: structs::DataDescriptor64::SIGNATURE,
                crc32: entry.crc32.unwrap(),
                compressed_size: entry.data.get_size(),
                uncompressed_size: entry.data.get_size(),
            },
            output,
        );

        true
    }

    fn read_cd_file_header<D: EntryData>(
        &mut self,
        entry: &ReaderEntry<D>,
        output: &mut ReadBuf<'_>,
    ) -> bool {
        // TODO: Recalculate CRC if needed
        self.read_packed_struct(
            structs::CentralDirectoryHeader {
                signature: structs::CentralDirectoryHeader::SIGNATURE,
                version_made_by: structs::VersionMadeBy {
                    os: structs::VersionMadeByOs::UNIX,
                    spec_version: ZIP64_VERSION_TO_EXTRACT as u8,
                },
                version_to_extract: ZIP64_VERSION_TO_EXTRACT,
                flags: 0,
                compression: structs::Compression::Store,
                last_mod_time: 0,
                last_mod_date: 0,
                crc32: entry.crc32.unwrap(),
                compressed_size: 0xffffffff,
                uncompressed_size: 0xffffffff,
                file_name_len: entry.name.len() as u16,
                extra_field_len: structs::Zip64ExtraField::packed_size() as u16,
                file_comment_length: 0,
                disk_number_start: 0xffff,
                internal_attributes: 0,
                external_attributes: 0,
                local_header_offset: 0xffffffff,
            },
            output,
        );
        self.read_str(&entry.name, output);
        self.read_packed_struct(
            structs::Zip64ExtraField {
                tag: structs::Zip64ExtraField::TAG,
                size: structs::Zip64ExtraField::packed_size() as u16 - 4,
                uncompressed_size: entry.data.get_size(),
                compressed_size: entry.data.get_size(),
                offset: entry.offset,
            },
            output,
        );

        true
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
                    os: structs::VersionMadeByOs::UNIX,
                    spec_version: ZIP64_VERSION_TO_EXTRACT as u8,
                },
                version_to_extract: ZIP64_VERSION_TO_EXTRACT,
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
                this_cd_entry_count: 0xffff,
                total_cd_entry_count: 0xffff,
                size_of_cd: 0xffffffff,
                cd_offset: 0xffffffff,
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

        println!();
        while output.remaining() > 0 {
            if !self.buffer.is_empty() {
                self.read_from_buffer(output);
                continue;
            }

            println!("Current chunk: {:?}", self.current_chunk);
            if let Chunk::Finished = self.current_chunk {
                return Poll::Ready(Ok(()));
            }

            dbg!(self.to_skip);

            let current_chunk_size = self.current_chunk.size(entries);
            /* TODO: Skipping is now disabled, because we don't have code for re-computing CRCs yet.
            if self.to_skip >= current_chunk_size {
                self.to_skip -= current_chunk_size;
                self.current_chunk = self.current_chunk.next(entries);
                continue;
            }
            */

            let loop_remaining = output.remaining();

            let chunk_done = match &mut self.current_chunk {
                Chunk::LocalHeader { entry_index } => {
                    let entry_index = *entry_index;
                    self.read_local_header(&entries[entry_index], output)
                }
                Chunk::FileData { entry_index } => {
                    let entry_index = *entry_index;
                    if output.remaining() != initial_remaining {
                        // We have already written something into the buffer -> interrupt this call, because
                        // we might need to return Pending when reading the file data
                        return Poll::Ready(Ok(()));
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
                    self.read_data_descriptor(&entries[entry_index], output)
                }
                Chunk::CDFileHeader { entry_index } => {
                    let entry_index = *entry_index;
                    self.read_cd_file_header(&entries[entry_index], output)
                }
                Chunk::EOCD => self.read_eocd(sizes, entries, output),
                _ => panic!("Unexpected current chunk"),
            };

            let read_len = loop_remaining - output.remaining();

            dbg!(read_len);
            dbg!(chunk_done);

            if chunk_done {
                assert!(self.chunk_processed_size == current_chunk_size);
                self.current_chunk = self.current_chunk.next(entries);
                self.chunk_processed_size = 0;
            }
        }

        Poll::Ready(Ok(()))
    }
}

impl<D: EntryData> Reader<D> {
    pub fn get_size(&self) -> u64 {
        self.sizes.total_size
    }
}

impl<D: EntryData> AsyncRead for Reader<D> {
    fn poll_read(
        self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let projected = self.project();
        projected.read_state.read(
            &projected.sizes,
            projected.entries.as_mut_slice(),
            projected.pinned,
            ctx,
            buf,
        )
    }
}

impl<D: EntryData> AsyncSeek for Reader<D> {
    fn start_seek(self: Pin<&mut Self>, _position: SeekFrom) -> std::io::Result<()> {
        todo!()
    }

    fn poll_complete(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<u64>> {
        todo!()
    }
}

#[derive(Clone, Debug, Error, PartialEq)]
pub enum ZippityError {
    #[error("Entry {entry_name} reports length {expected_size} B, but was {actual_size} B")]
    LengthMismatch {
        entry_name: String,
        expected_size: u64,
        actual_size: u64,
    },
}

#[cfg(test)]
mod test {
    use crate::test_util::{nonempty_range_strategy, read_size_strategy, read_to_vec, unasync};

    use super::*;
    use assert2::assert;
    use proptest::strategy::{Just, Strategy};
    use std::{collections::HashMap, fmt::format, io::ErrorKind, ops::Range, pin::pin};
    use test_strategy::proptest;
    use zip::read::ZipArchive;

    fn content_strategy() -> impl Strategy<Value = HashMap<String, Vec<u8>>> {
        proptest::collection::hash_map(
            // We're limiting the character set significantly, because the zip crate we use for verification
            // does not handle unicode filenames well.
            //r"[a-zA-Z0-91235678!@#$%^&U*/><\\\[\]]{1,20}",
            // TODO: So far there seems to not be many problems with it...
            ".{1,20}",
            proptest::collection::vec(proptest::bits::u8::ANY, 0..100),
            0..10,
        )
    }

    #[proptest]
    fn read_packed_struct(
        #[strategy(nonempty_range_strategy(9))] range: Range<usize>,
        nonempty_buffer: bool,
    ) {
        // TODO: I hate this test, although it seems to work well. It's too complicated and hard to understand.
        use packed_struct::prelude::*;
        #[derive(Debug, PackedStruct)]
        #[packed_struct(endian = "msb")]
        struct TestPs {
            v: u64,
        }

        let mut rs = ReadState {
            current_chunk: Chunk::Finished,
            chunk_processed_size: 0,
            buffer: if nonempty_buffer {
                vec![0xaa]
            } else {
                Vec::new()
            },
            to_skip: range.start as u64,
        };

        let expected_backing = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let read_all = range.end >= expected_backing.len();
        let cleaned_range = if read_all {
            range.start..expected_backing.len()
        } else {
            range.clone()
        };
        let expected = &expected_backing[cleaned_range.clone()];

        let mut buf_backing = [0; 9];
        let mut buf = ReadBuf::new(&mut buf_backing[..range.len()]);

        rs.read_packed_struct(
            TestPs {
                v: 0x0102030405060708,
            },
            &mut buf,
        );

        if rs.buffer.is_empty() {
            assert!(buf.filled() == expected);
        } else {
            let start = rs.to_skip + if nonempty_buffer { 1 } else { 0 };
            assert!(
                &rs.buffer[(start as usize)..(start as usize + cleaned_range.len())] == expected
            );
            assert!(buf.filled().is_empty());
        }
    }

    #[proptest]
    fn read_str(
        #[strategy(".*".prop_flat_map(|s| {
            let len = s.bytes().len();
            (Just(s), nonempty_range_strategy(len + 1))
        }))]
        s_and_range: (String, Range<usize>),
        nonempty_buffer: bool,
    ) {
        // TODO: I hate this test, although it seems to work well. It's too complicated and hard to understand.
        let (s, range) = s_and_range;

        let bytes = s.as_bytes();
        let mut rs = ReadState {
            current_chunk: Chunk::Finished,
            chunk_processed_size: 0,
            buffer: if nonempty_buffer {
                vec![0xaa]
            } else {
                Vec::new()
            },
            to_skip: range.start as u64,
        };

        let read_all = range.end >= bytes.len();
        let cleaned_range = if read_all {
            range.start..bytes.len()
        } else {
            range.clone()
        };
        let expected = &bytes[cleaned_range.clone()];

        let mut buf_backing = Vec::new();
        buf_backing.resize(range.len(), 0);
        let mut buf = ReadBuf::new(buf_backing.as_mut_slice());

        rs.read_str(&s, &mut buf);

        if rs.buffer.is_empty() {
            assert!(buf.filled() == expected);
        } else {
            let start = rs.to_skip + if nonempty_buffer { 1 } else { 0 };
            assert!(
                &rs.buffer[(start as usize)..(start as usize + cleaned_range.len())] == expected
            );
            assert!(buf.filled().is_empty());
        }
    }

    #[proptest]
    fn read_from_buffer(
        #[strategy(proptest::collection::vec(proptest::bits::u8::ANY, 1..1000).prop_flat_map(|v| {
            let len = v.len();
            (Just(v), nonempty_range_strategy(len + 1))
        }))]
        v_and_range: (Vec<u8>, Range<usize>),
    ) {
        let (v, range) = v_and_range;

        let mut rs = ReadState {
            current_chunk: Chunk::Finished,
            chunk_processed_size: 0,
            buffer: v.clone(),
            to_skip: range.start as u64,
        };

        let read_all = range.end >= rs.buffer.len();
        let cleaned_range = if read_all {
            range.start..rs.buffer.len()
        } else {
            range.clone()
        };

        let mut buf_backing = Vec::new();
        buf_backing.resize(range.len(), 0);
        let mut buf = ReadBuf::new(buf_backing.as_mut_slice());

        rs.read_from_buffer(&mut buf);

        assert!(buf.filled() == &v[cleaned_range]);
    }

    #[proptest]
    fn test_empty_archive(#[strategy(read_size_strategy())] read_size: usize) {
        let zippity = pin!(Builder::<()>::new().build());
        let size = zippity.get_size();

        let buf = unasync(read_to_vec(zippity, read_size)).unwrap();

        assert!(size == (buf.len() as u64));

        let unpacked = ZipArchive::new(std::io::Cursor::new(buf)).expect("Should be a valid zip");
        assert!(unpacked.is_empty());
    }

    #[proptest]
    fn test_unzip_with_data(
        #[strategy(content_strategy())] content: HashMap<String, Vec<u8>>,
        #[strategy(read_size_strategy())] read_size: usize,
    ) {
        let mut builder: Builder<&[u8]> = Builder::new();

        content.iter().for_each(|(name, value)| {
            builder.add_entry(name.clone(), value.as_ref());
        });

        let zippity = pin!(builder.build());
        let size = zippity.get_size();

        let buf = unasync(read_to_vec(zippity, read_size)).unwrap();

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
    }

    #[test]
    fn bad_size() {
        /// Struct that reports data size 100, but actually its 1
        struct BadSize();
        impl EntryData for BadSize {
            type Reader = std::io::Cursor<&'static [u8]>;
            type ReaderFuture = std::future::Ready<Result<Self::Reader>>;

            fn get_size(&self) -> u64 {
                100
            }

            fn get_reader(&self) -> Self::ReaderFuture {
                std::future::ready(Ok(std::io::Cursor::new(&[5])))
            }
        }

        let mut builder: Builder<BadSize> = Builder::new();
        builder.add_entry("xxx".into(), BadSize());

        let zippity = pin!(builder.build());
        let e = unasync(read_to_vec(zippity, 1024)).unwrap_err();

        assert!(e.kind() == ErrorKind::Other);
        let message = format!("{}", e.into_inner().unwrap());

        assert!(message.contains("xxx"));
    }

    #[proptest]
    fn local_size_matches_chunks(
        #[strategy(content_strategy())] content: HashMap<String, Vec<u8>>,
    ) {
        let mut builder: Builder<&[u8]> = Builder::new();

        content.iter().for_each(|(name, value)| {
            builder.add_entry(name.clone(), value.as_ref());
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
}
