use lru::LruCache;
use packed_struct::{PackedStruct, PackedStructSlice};
use pin_project::pin_project;
use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use structs::PackedStructZippityExt;
use tokio::{
    io::{AsyncRead, AsyncSeek, ReadBuf},
    sync::Mutex,
};
use vecbuffer::VecBuffer;

mod structs;
mod vecbuffer;

/// Minimum version needed to extract the zip64 extensions required by zippity
pub const ZIP64_VERSION_TO_EXTRACT: u16 = 45;

pub trait EntryData {
    type Reader: AsyncRead;

    fn get_size(&self) -> u64;
    fn get_reader(&self) -> Self::Reader;
}

#[derive(Debug, Hash, Clone, PartialEq, Eq)]
struct CrcCacheKey {}

pub struct CrcCache(Mutex<LruCache<CrcCacheKey, u32>>);

impl CrcCache {
    pub fn new(limit: NonZeroUsize) -> Self {
        CrcCache(Mutex::new(LruCache::new(limit)))
    }

    pub fn unbounded() -> Self {
        CrcCache(Mutex::new(LruCache::unbounded()))
    }
}

impl EntryData for () {
    type Reader = std::io::Cursor<&'static [u8]>;

    fn get_size(&self) -> u64 {
        0
    }

    fn get_reader(&self) -> Self::Reader {
        std::io::Cursor::new(&[])
    }
}

/*impl<T> EntryData for T
where
    T: AsRef<[u8]> + Unpin,
{
    type Reader = std::io::Cursor<&[u8]>;

    fn get_size(&self) -> u64 {
        self.as_ref().len() as u64
    }

    fn get_reader(&self) -> Self::Reader {
        std::io::Cursor::new(self.as_ref())
    }
}*/

#[derive(Clone, Debug)]
struct BuilderEntry<D> {
    data: D,
}

impl<D: EntryData> BuilderEntry<D> {
    fn get_local_size(&self, name: &str) -> u64 {
        let local_header = structs::LocalFileHeader::packed_size();
        let filename = name.len() as u64;
        let data = self.data.get_size();
        //let data_descriptor = structs::DataDescriptor64::packed_size();

        local_header + filename + data
    }

    fn get_cd_header_size(&self, name: &str) -> u64 {
        let filename = name.len() as u64;
        let cd_entry = structs::CentralDirectoryHeader::packed_size();
        cd_entry + filename
    }
}

#[derive(Clone, Debug)]
struct ReaderEntry<D> {
    name: String,
    data: D,
    size: u64,
    offset: u64,
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

    pub fn build<'a>(self, crc_cache: &'a CrcCache) -> Reader<'a, D> {
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
                }
            })
            .collect();

        let cd_offset = offset;
        let total_size = cd_offset + cd_size + structs::EndOfCentralDirectory::packed_size();
        let state = ZipPiece::new(&entries);

        Reader {
            entries,

            cd_offset,
            cd_size,
            total_size,

            state,
            buffer: VecBuffer::new(),

            crc_cache,
        }
    }
}

#[derive(Debug)]
enum ZipPiece {
    LocalHeader(usize),
    LocalHeaderFilename {
        entry_index: usize,
        bytes_written: usize,
    },
    FileData(usize), // TODO: Hasher
    DataDescriptor(usize),
    CDFileHeader(usize),
    CDFileHeaderFilename {
        entry_index: usize,
        bytes_written: usize,
    },
    CDEnd,
    Finished,
}

impl ZipPiece {
    fn new<D>(entries: &Vec<ReaderEntry<D>>) -> ZipPiece {
        if entries.is_empty() {
            ZipPiece::CDEnd
        } else {
            ZipPiece::LocalHeader(0)
        }
    }
}

#[pin_project]
pub struct Reader<'a, D: EntryData> {
    /// Vector of entries and their offsets  (counted from start of file)
    entries: Vec<ReaderEntry<D>>,

    cd_offset: u64,
    cd_size: u64,
    total_size: u64,

    state: ZipPiece,
    buffer: VecBuffer,

    crc_cache: &'a CrcCache,
}

impl<'a, D: EntryData> Reader<'a, D> {
    pub fn get_size(&self) -> u64 {
        self.total_size
    }

    fn make_local_header(&self, entry_index: usize) -> impl PackedStruct {
        structs::LocalFileHeader {
            signature: structs::LocalFileHeader::SIGNATURE,
            version_to_extract: ZIP64_VERSION_TO_EXTRACT,
            flags: 0,
            compression: structs::Compression::Store,
            last_mod_time: 0,
            last_mod_date: 0,
            crc32: 0,
            compressed_size: 0,
            uncompressed_size: 0,
            file_name_len: self.entries[entry_index].name.len() as u16,
            extra_field_len: 0,
        }
    }

    fn make_cd_header(&self, entry_index: usize) -> impl PackedStruct {
        let entry = &self.entries[entry_index];
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
            crc32: 0,
            compressed_size: 0,
            uncompressed_size: 0,
            file_name_len: entry.name.len() as u16,
            extra_field_len: 0,
            file_comment_length: 0,
            disk_number_start: 0,
            internal_attributes: 0,
            external_attributes: 0,
            local_header_offset: entry.offset as u32,
        }
    }

    fn make_eocd(&self) -> impl PackedStruct {
        structs::EndOfCentralDirectory {
            signature: structs::EndOfCentralDirectory::SIGNATURE,
            this_disk_number: 0,
            start_of_cd_disk_number: 0,
            this_cd_entry_count: self.entries.len() as u16,
            total_cd_entry_count: self.entries.len() as u16,
            size_of_cd: self.cd_size as u32,
            cd_offset: self.cd_offset as u32,
            file_comment_length: 0,
        }
    }

    fn write_packed_struct<T: PackedStruct>(
        mut self: Pin<&mut Self>,
        ps: T,
        output: &mut ReadBuf<'_>,
    ) {
        assert!(!self.buffer.has_remaining());

        let size = T::packed_size() as usize;

        if output.remaining() >= size {
            let buf_slice = output.initialize_unfilled_to(size);
            ps.pack_to_slice(buf_slice).unwrap();
            output.advance(size);
        } else {
            let buf_slice = self.buffer.reset(size);
            ps.pack_to_slice(buf_slice).unwrap();

            self.buffer.read_into_readbuf(output);
        }
    }

    fn write_filename(
        &self,
        entry_index: usize,
        bytes_written: usize,
        output: &mut ReadBuf<'_>,
    ) -> Option<usize> {
        let bytes = self.entries[entry_index].name.as_bytes();
        let bytes = &bytes[bytes_written..];

        if bytes.len() <= output.remaining() {
            output.put_slice(bytes);
            None
        } else {
            let new_bytes_written = bytes_written + output.remaining();
            output.put_slice(&bytes[..output.remaining()]);
            Some(new_bytes_written)
        }
    }
}

impl<'a, D: EntryData> AsyncRead for Reader<'a, D> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        // TODO: Don't return immediately after every write, keep looping
        loop {
            // If there is a part of a struct in the buffer waiting to be written,
            // handle that first
            if self.buffer.has_remaining() {
                self.buffer.read_into_readbuf(buf);
                return Poll::Ready(Ok(()));
            }

            match self.state {
                ZipPiece::LocalHeader(entry_index) => {
                    let lh = self.make_local_header(entry_index);
                    self.as_mut().write_packed_struct(lh, buf);
                    self.state = ZipPiece::LocalHeaderFilename {
                        entry_index,
                        bytes_written: 0,
                    };
                }
                ZipPiece::LocalHeaderFilename {
                    entry_index,
                    bytes_written,
                } => {
                    if let Some(new_bytes_written) =
                        self.write_filename(entry_index, bytes_written, buf)
                    {
                        self.state = ZipPiece::LocalHeaderFilename {
                            entry_index,
                            bytes_written: new_bytes_written,
                        };
                        return Poll::Ready(Ok(()));
                    } else {
                        let entry_index = entry_index + 1;
                        self.state = if entry_index < self.entries.len() {
                            ZipPiece::LocalHeader(entry_index)
                        } else {
                            ZipPiece::CDFileHeader(0)
                        };
                    }
                }
                ZipPiece::FileData(entry_index) => (),
                ZipPiece::DataDescriptor(entry_index) => (),
                ZipPiece::CDFileHeader(entry_index) => {
                    let lh = self.make_cd_header(entry_index);
                    self.as_mut().write_packed_struct(lh, buf);
                    self.state = ZipPiece::CDFileHeaderFilename {
                        entry_index,
                        bytes_written: 0,
                    };
                }
                ZipPiece::CDFileHeaderFilename {
                    entry_index,
                    bytes_written,
                } => {
                    if let Some(new_bytes_written) =
                        self.write_filename(entry_index, bytes_written, buf)
                    {
                        self.state = ZipPiece::CDFileHeaderFilename {
                            entry_index,
                            bytes_written: new_bytes_written,
                        };
                        return Poll::Ready(Ok(()));
                    } else {
                        let entry_index = entry_index + 1;
                        self.state = if entry_index < self.entries.len() {
                            ZipPiece::CDFileHeader(entry_index)
                        } else {
                            ZipPiece::CDEnd
                        };
                    }
                }
                ZipPiece::CDEnd => {
                    let eocd = self.make_eocd();
                    self.as_mut().write_packed_struct(eocd, buf);
                    self.state = ZipPiece::Finished;
                    return Poll::Ready(Ok(()));
                }
                _ => {
                    return Poll::Ready(Ok(()));
                }
            }
            //            ready!(self.as_mut().state.poll_read(&self.entries, ctx, buf))?;
        }
    }
}

/*
impl<'a, D: EntryData> AsyncSeek for ArchiveReader<'a, D> {
    fn start_seek(self: Pin<&mut Self>, position: SeekFrom) -> std::io::Result<()> {}

    fn poll_complete(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<u64>> {}
}*/

#[cfg(test)]
mod test {
    use super::*;
    use assert2::assert;
    use proptest::strategy::{Just, Strategy};
    use std::{collections::HashMap, future::Future};
    use test_strategy::proptest;
    use tokio::io::AsyncReadExt;
    use zip::read::ZipArchive;

    async fn read_to_vec(reader: impl AsyncRead, read_size: usize) -> Vec<u8> {
        let mut buffer = Vec::new();
        let mut reader = Box::pin(reader);

        loop {
            let size_before = buffer.len();
            buffer.resize(size_before + read_size, 0);
            let (_, write_slice) = buffer.split_at_mut(size_before);

            let size_read = reader.read(write_slice).await.unwrap();

            buffer.truncate(size_before + size_read);

            if size_read == 0 {
                return buffer;
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

    fn content_strategy() -> impl Strategy<Value = HashMap<String, ()>> {
        proptest::collection::hash_map(
            // We're limiting the character set significantly, because the zip crate we use for verification
            // does not handle unicode filenames well.
            r"[a-zA-Z0-91235678!@#$%^&U*/><\\\[\]]{1,1000}",
            Just(()),
            0..100,
        )
    }

    #[proptest]
    fn empty(#[strategy(1usize..8192usize)] read_size: usize) {
        let cache = CrcCache::unbounded();
        let zippity: Reader<()> = Builder::new().build(&cache);
        let size = zippity.get_size();

        let buf = unasync(read_to_vec(zippity, read_size));

        assert!(size == (buf.len() as u64));

        let unpacked = ZipArchive::new(std::io::Cursor::new(buf)).expect("Should be a valid zip");
        assert!(unpacked.is_empty());
    }

    #[proptest]
    fn alltest(
        #[strategy(content_strategy())] content: HashMap<String, ()>,
        #[strategy(1usize..8192usize)] read_size: usize,
    ) {
        let mut builder = Builder::new();

        content.iter().for_each(|(name, _value)| {
            builder.add_entry(name.clone(), ());
        });

        let cache = CrcCache::unbounded();
        let zippity: Reader<()> = builder.build(&cache);
        let size = zippity.get_size();

        let buf = unasync(read_to_vec(zippity, read_size));

        assert!(size == (buf.len() as u64));

        let unpacked = ZipArchive::new(std::io::Cursor::new(buf)).expect("Should be a valid zip");
        let unpacked_names = unpacked
            .file_names()
            .map(|name| (name.to_string(), ()))
            .collect::<HashMap<_, _>>();

        assert!(unpacked_names == content);
    }
}
