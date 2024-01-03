use lru::LruCache;
use packed_struct::{PackedStruct, PackedStructSlice};
use pin_project::pin_project;
use std::collections::BTreeMap;
use std::future::Future;
use std::io::{Error, Result};
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::task::{Context, Poll};
use structs::PackedStructZippityExt;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncSeek, ReadBuf};
use vecbuffer::VecBuffer;

mod structs;
mod vecbuffer;

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
        let filename = name.len() as u64;
        let data = self.data.get_size();
        let data_descriptor = structs::DataDescriptor32::packed_size();

        let size = local_header + filename + data + data_descriptor;
        size
    }

    fn get_cd_header_size(&self, name: &str) -> u64 {
        let filename = name.len() as u64;
        let cd_entry = structs::CentralDirectoryHeader::packed_size();

        let size = cd_entry + filename;
        size
    }
}

#[derive(Clone, Debug)]
struct ReaderEntry<D> {
    name: String,
    data: D,
    size: u64,
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
                    size,
                    offset: offset_copy,
                    crc32: None,
                }
            })
            .collect();

        let cd_offset = offset;
        let eocd_size = structs::EndOfCentralDirectory::packed_size();
        let total_size = cd_offset + cd_size + eocd_size;
        let state = ReaderState::new(&entries);

        Reader {
            entries,

            cd_offset,
            cd_size,
            total_size,

            state,
            pinned: ReaderPinned::Nothing,
            buffer: VecBuffer::new(),
        }
    }
}

#[derive(Debug)]
enum ReaderState {
    LocalHeader(usize),
    LocalHeaderFilename(usize),
    WaitForReader(usize),
    FileData {
        entry_index: usize,
        hasher: crc32fast::Hasher,
        size: u64,
    },
    DataDescriptor(usize),
    CDFileHeader(usize),
    CDFileHeaderFilename(usize),
    CDEnd,
    Finished,
}

impl ReaderState {
    fn new<D>(entries: &Vec<ReaderEntry<D>>) -> ReaderState {
        if entries.is_empty() {
            ReaderState::CDEnd
        } else {
            ReaderState::LocalHeader(0)
        }
    }
}

#[pin_project(project = ReaderPinnedProj)]
enum ReaderPinned<D: EntryData> {
    Nothing,
    ReaderFuture(#[pin] D::ReaderFuture),
    FileReader(#[pin] D::Reader),
}

#[pin_project]
pub struct Reader<D: EntryData> {
    /// Vector of entries and their offsets  (counted from start of file)
    entries: Vec<ReaderEntry<D>>,

    cd_offset: u64,
    cd_size: u64,
    total_size: u64,

    state: ReaderState,
    #[pin]
    pinned: ReaderPinned<D>,
    buffer: VecBuffer,
}

/// Write as much of ps as possible into output, spill the rest to overflow
/// Overflow has to be empty.
fn write_packed_struct<P: PackedStruct>(ps: P, output: &mut ReadBuf<'_>, overflow: &mut VecBuffer) {
    assert!(!overflow.has_remaining());

    let size = P::packed_size() as usize;

    if output.remaining() >= size {
        let buf_slice = output.initialize_unfilled_to(size);
        ps.pack_to_slice(buf_slice).unwrap();
        output.advance(size);
    } else {
        let buf_slice = overflow.reset(size);
        ps.pack_to_slice(buf_slice).unwrap();

        overflow.read_into_readbuf(output);
    }
}

/// Write as much of a string slice as possible into output, spill the rest to overflow
/// Overflow has to be empty.
fn write_slice(slice: &str, output: &mut ReadBuf<'_>, overflow: &mut VecBuffer) {
    assert!(!overflow.has_remaining());

    let bytes = slice.as_bytes();

    if bytes.len() <= output.remaining() {
        output.put_slice(bytes);
    } else {
        let (to_output, to_overflow) = bytes.split_at(output.remaining());
        output.put_slice(to_output);
        overflow
            .reset(to_overflow.len())
            .copy_from_slice(to_overflow);
    }
}

impl<D: EntryData> Reader<D> {
    pub fn get_size(&self) -> u64 {
        self.total_size
    }
}

impl<D: EntryData> ReaderEntry<D> {
    fn make_local_header(&self) -> impl PackedStruct {
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
            compressed_size: 0,
            uncompressed_size: 0,
            file_name_len: self.name.len() as u16,
            extra_field_len: 0,
        }
    }

    fn make_data_descriptor32(&self) -> impl PackedStruct {
        structs::DataDescriptor32 {
            signature: structs::DataDescriptor32::SIGNATURE,
            crc32: self.crc32.unwrap(),
            compressed_size: self.data.get_size() as u32, // TODO: zip64
            uncompressed_size: self.data.get_size() as u32, // TODO: zip64
        }
    }

    fn make_cd_header(&self) -> impl PackedStruct {
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
            crc32: self.crc32.unwrap(),
            compressed_size: self.data.get_size() as u32, // TODO: zip64
            uncompressed_size: self.data.get_size() as u32, // TODO: zip64
            file_name_len: self.name.len() as u16,
            extra_field_len: 0,
            file_comment_length: 0,
            disk_number_start: 0,
            internal_attributes: 0,
            external_attributes: 0,
            local_header_offset: self.offset as u32,
        }
    }
}

fn make_eocd<D: EntryData>(
    entries: &Vec<ReaderEntry<D>>,
    cd_size: u64,
    cd_offset: u64,
) -> impl PackedStruct {
    structs::EndOfCentralDirectory {
        signature: structs::EndOfCentralDirectory::SIGNATURE,
        this_disk_number: 0,
        start_of_cd_disk_number: 0,
        this_cd_entry_count: entries.len() as u16,
        total_cd_entry_count: entries.len() as u16,
        size_of_cd: cd_size as u32,
        cd_offset: cd_offset as u32,
        file_comment_length: 0,
    }
}

impl<D: EntryData> AsyncRead for Reader<D> {
    fn poll_read(
        self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let mut projected = self.project();
        let initial_remaining = buf.remaining();

        while buf.remaining() > 0 {
            // If there is a part of a struct in the buffer waiting to be written,
            // handle that first
            if projected.buffer.has_remaining() {
                projected.buffer.read_into_readbuf(buf);
                return Poll::Ready(Ok(()));
            }

            match projected.state {
                ReaderState::LocalHeader(entry_index) => {
                    let entry = &projected.entries[*entry_index];
                    write_packed_struct(entry.make_local_header(), buf, &mut projected.buffer);
                    *projected.state = ReaderState::LocalHeaderFilename(*entry_index);
                }
                ReaderState::LocalHeaderFilename(entry_index) => {
                    let entry = &projected.entries[*entry_index];
                    write_slice(&entry.name, buf, &mut projected.buffer);

                    projected
                        .pinned
                        .set(ReaderPinned::ReaderFuture(entry.data.get_reader()));
                    *projected.state = ReaderState::WaitForReader(*entry_index);
                }
                ReaderState::WaitForReader(entry_index) => {
                    let ReaderPinnedProj::ReaderFuture(future) =
                        projected.pinned.as_mut().project()
                    else {
                        panic!("Wrong pinned future");
                    };

                    match future.poll(ctx) {
                        Poll::Ready(Ok(reader)) => {
                            projected.pinned.set(ReaderPinned::FileReader(reader));
                            *projected.state = ReaderState::FileData {
                                entry_index: *entry_index,
                                hasher: crc32fast::Hasher::new(),
                                size: 0,
                            };
                        }
                        Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                        Poll::Pending => {
                            if buf.remaining() == initial_remaining {
                                return Poll::Pending;
                            } else {
                                return Poll::Ready(Ok(()));
                            }
                        }
                    }
                }
                ReaderState::FileData {
                    entry_index,
                    hasher,
                    size,
                } => {
                    let ReaderPinnedProj::FileReader(reader) = projected.pinned.as_mut().project()
                    else {
                        panic!("Wrong pinned future");
                    };

                    let remaining_before_poll = buf.remaining();
                    match reader.poll_read(ctx, buf) {
                        Poll::Ready(Ok(())) => {
                            if buf.remaining() == remaining_before_poll {
                                // Nothing was output => we read everything in the file already

                                let entry = &mut projected.entries[*entry_index];
                                // Cloning as a workaround -- finalize consumes, but we only borrowed the hasher mutably
                                entry.crc32 = Some(hasher.clone().finalize());

                                let reported_length = entry.data.get_size();
                                if *size != reported_length {
                                    return Poll::Ready(Err(Error::other(Box::new(
                                        ZippityError::LengthMismatch {
                                            entry_name: entry.name.clone(),
                                            reported_length,
                                            actual_length: *size,
                                        },
                                    ))));
                                }

                                projected.pinned.set(ReaderPinned::Nothing);
                                *projected.state = ReaderState::DataDescriptor(*entry_index);
                            } else {
                                let written_chunk_size = remaining_before_poll - buf.remaining();
                                let buf_slice = buf.filled();
                                let written_chunk =
                                    &buf_slice[(buf_slice.len() - written_chunk_size)..];
                                assert!(written_chunk_size == written_chunk.len());

                                hasher.update(written_chunk);
                                *size += written_chunk.len() as u64;
                            }
                        }
                        Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                        Poll::Pending => {
                            if buf.remaining() == initial_remaining {
                                return Poll::Pending;
                            } else {
                                return Poll::Ready(Ok(()));
                            }
                        }
                    }
                }
                ReaderState::DataDescriptor(entry_index) => {
                    let entry = &projected.entries[*entry_index];
                    write_packed_struct(entry.make_data_descriptor32(), buf, projected.buffer);
                    let entry_index = *entry_index + 1;
                    *projected.state = if entry_index < projected.entries.len() {
                        ReaderState::LocalHeader(entry_index)
                    } else {
                        ReaderState::CDFileHeader(0)
                    }
                }
                ReaderState::CDFileHeader(entry_index) => {
                    let entry = &projected.entries[*entry_index];
                    write_packed_struct(entry.make_cd_header(), buf, projected.buffer);
                    *projected.state = ReaderState::CDFileHeaderFilename(*entry_index);
                }
                ReaderState::CDFileHeaderFilename(entry_index) => {
                    let entry = &projected.entries[*entry_index];
                    write_slice(&entry.name, buf, projected.buffer);

                    let entry_index = *entry_index + 1;
                    *projected.state = if entry_index < projected.entries.len() {
                        ReaderState::CDFileHeader(entry_index)
                    } else {
                        ReaderState::CDEnd
                    };
                }
                ReaderState::CDEnd => {
                    let eocd =
                        make_eocd(projected.entries, *projected.cd_size, *projected.cd_offset);
                    write_packed_struct(eocd, buf, projected.buffer);
                    *projected.state = ReaderState::Finished;
                }
                _ => {
                    return Poll::Ready(Ok(()));
                }
            }
        }

        Poll::Ready(Ok(()))
    }
}

/*
impl<'a, D: EntryData> AsyncSeek for ArchiveReader<'a, D> {
    fn start_seek(self: Pin<&mut Self>, position: SeekFrom) -> std::io::Result<()> {}

    fn poll_complete(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<u64>> {}
}*/

#[derive(Clone, Debug, Error, PartialEq)]
pub enum ZippityError {
    #[error("Entry {entry_name} reports length {reported_length} B, but was {actual_length} B")]
    LengthMismatch {
        entry_name: String,
        reported_length: u64,
        actual_length: u64,
    },
}

#[cfg(test)]
mod test {
    use super::*;
    use assert2::assert;
    use proptest::strategy::{Just, Strategy};
    use std::{collections::HashMap, fmt::format, future::Future, io::ErrorKind};
    use test_strategy::proptest;
    use tokio::io::AsyncReadExt;
    use zip::read::ZipArchive;

    async fn read_to_vec(reader: impl AsyncRead, read_size: usize) -> Result<Vec<u8>> {
        let mut buffer = Vec::new();
        let mut reader = Box::pin(reader);

        loop {
            let size_before = buffer.len();
            buffer.resize(size_before + read_size, 0);
            let (_, write_slice) = buffer.split_at_mut(size_before);

            let size_read = reader.read(write_slice).await?;

            buffer.truncate(size_before + size_read);

            if size_read == 0 {
                return Ok(buffer);
            }
        }
    }

    fn unasync<Fut: Future>(fut: Fut) -> Fut::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(fut)
    }

    fn content_strategy() -> impl Strategy<Value = HashMap<String, Vec<u8>>> {
        proptest::collection::hash_map(
            // We're limiting the character set significantly, because the zip crate we use for verification
            // does not handle unicode filenames well.
            r"[a-zA-Z0-91235678!@#$%^&U*/><\\\[\]]{1,500}",
            proptest::collection::vec(0u8..255u8, 0..1024),
            0..100,
        )
    }

    #[proptest]
    fn test_empty_archive(#[strategy(1usize..8192usize)] read_size: usize) {
        let zippity: Reader<()> = Builder::new().build();
        let size = zippity.get_size();

        let buf = unasync(read_to_vec(zippity, read_size)).unwrap();

        assert!(size == (buf.len() as u64));

        let unpacked = ZipArchive::new(std::io::Cursor::new(buf)).expect("Should be a valid zip");
        assert!(unpacked.is_empty());
    }

    #[proptest]
    fn test_unzip_with_data(
        #[strategy(content_strategy())] content: HashMap<String, Vec<u8>>,
        #[strategy(1usize..8192usize)] read_size: usize,
    ) {
        let mut builder: Builder<&[u8]> = Builder::new();

        content.iter().for_each(|(name, value)| {
            builder.add_entry(name.clone(), value.as_ref());
        });

        let zippity = builder.build();
        let size = zippity.get_size();

        let buf = unasync(read_to_vec(zippity, read_size)).unwrap();

        assert!(size == (buf.len() as u64));

        let mut unpacked =
            ZipArchive::new(std::io::Cursor::new(buf)).expect("Should be a valid zip");
        assert!(unpacked.len() == content.len());

        let mut unpacked_content = HashMap::new();
        for i in 0..unpacked.len() {
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

        let zippity = builder.build();
        let e = unasync(read_to_vec(zippity, 1024)).unwrap_err();

        assert!(e.kind() == ErrorKind::Other);
        let message = format!("{}", e.into_inner().unwrap());

        assert!(message.contains("xxx"));
    }
}
