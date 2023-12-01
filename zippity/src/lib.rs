use lru::LruCache;
use packed_struct::{PackedStructInfo, PackedStructSlice};
use pin_project::pin_project;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::task::{ready, Context, Poll};
use tokio::{
    io::{AsyncRead, AsyncSeek, ReadBuf},
    sync::Mutex,
};
use vecbuffer::VecBuffer;

mod structs;
mod vecbuffer;

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
    name: String,
    data: D,
}

impl<D: EntryData> BuilderEntry<D> {
    fn get_size(&self) -> u64 {
        let local_header = (structs::LocalFileHeader::packed_bits() as u64) / 8;
        let filename = self.name.len() as u64;
        let data = self.data.get_size();
        let data_descriptor = (structs::DataDescriptor64::packed_bits() as u64) / 8;

        local_header + filename + data + data_descriptor
    }

    fn get_cd_entry_size(&self) -> u64 {
        todo!()
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
    entries: Vec<BuilderEntry<D>>,
}

impl<D: EntryData> Builder<D> {
    pub fn new() -> Self {
        Builder {
            entries: Vec::new(),
        }
    }

    pub fn add_entry<T: Into<D>>(&mut self, name: String, data: T) {
        let data = data.into();
        self.entries.push(BuilderEntry {
            name,
            data: data.into(),
        });
    }

    fn get_cd_size(&self) -> u64 {
        //let entries = self.entries.iter().map(|e| e.get_cd_entry_size()).sum();
        //let zip64_eocd = 0;
        //let zip64_eocd_locator = 0;

        0
    }

    pub fn build<'a>(self, crc_cache: &'a CrcCache) -> Reader<'a, D> {
        let cd_size = self.get_cd_size();
        let entries: Vec<_> = self
            .entries
            .into_iter()
            .scan(0, |accumulator, entry| {
                let size = entry.data.get_size() + (entry.name.len() as u64) + 50; // TODO: Fix the structures size!
                let offset = *accumulator;
                *accumulator += size;
                Some(ReaderEntry {
                    name: entry.name,
                    data: entry.data,
                    size,
                    offset,
                })
            })
            .collect();

        let total_size = entries.last().map_or(0, |entry| entry.offset + entry.size)
            + cd_size
            + (structs::EndOfCentralDirectory::packed_bytes_size(None).unwrap() as u64);
        let state = ZipPiece::new(&entries);

        Reader {
            entries,

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
    FileData(usize), // TODO: Hasher
    DataDescriptor(usize),
    CDFileHeader(usize),
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

    fn make_eocd(&self) -> impl PackedStructSlice {
        structs::EndOfCentralDirectory {
            signature: structs::EndOfCentralDirectory::SIGNATURE,
            this_disk_number: 0,
            start_of_cd_disk_number: 0,
            this_cd_entry_count: self.entries.len() as u16,
            total_cd_entry_count: self.entries.len() as u16,
            size_of_cd: self.cd_size as u32,
            offset_of_cd_start: 0,
            file_comment_length: 0,
        }
    }

    fn write_packed_struct(
        mut self: Pin<&mut Self>,
        ps: impl PackedStructSlice,
        output: &mut ReadBuf<'_>,
    ) {
        assert!(!self.buffer.has_remaining());

        let size = PackedStructSlice::packed_bytes_size(Some(&ps)).unwrap();

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
}

impl<'a, D: EntryData> AsyncRead for Reader<'a, D> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        ctx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        loop {
            // If there is a part of a struct in the buffer waiting to be written,
            // handle that first
            if self.buffer.has_remaining() {
                self.buffer.read_into_readbuf(buf);
                return Poll::Ready(Ok(()));
            }

            match self.state {
                /*ZipPiece::LocalHeader(entry_index) => (),
                ZipPiece::FileData(entry_index) => (),
                ZipPiece::DataDescriptor(entry_index) => Some(ZipPiece::CDFileHeader(0)),
                ZipPiece::CDFileHeader(_) => Some(ZipPiece::CDEnd),
                */
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
    use proptest::prop_assume;
    use std::future::Future;
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
}
