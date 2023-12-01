use tokio::io::ReadBuf;

/// Buffer that can be written in one operation and then read
/// in smaller steps,
/// Internally keeps a cursor of the already read data.
#[derive(Debug)]
pub struct VecBuffer {
    buf: Vec<u8>,
    cursor: usize,
}

impl VecBuffer {
    /// Create an empty buffer
    pub fn new() -> VecBuffer {
        VecBuffer {
            buf: Vec::new(),
            cursor: 0,
        }
    }

    /// Resize buffer to  the requested size, reset the cursor,
    /// Returns a slice of the requested size, that can be used to
    /// write the new content of the buffer.
    /// The returned slice might contain previous data from the buffer
    pub fn reset(&mut self, size: usize) -> &mut [u8] {
        self.buf.resize(size, 0);
        self.cursor = 0;
        self.buf.as_mut_slice()
    }

    /// Read at most max_size bytes from the buffer,
    /// returning the data as slice.
    pub fn read(&mut self, max_size: usize) -> &[u8] {
        let end_index = self.buf.len().min(self.cursor + max_size);
        let data = self.buf.as_slice().get(self.cursor..end_index).unwrap();
        self.cursor = end_index;
        data
    }

    pub fn read_into_readbuf(&mut self, target: &mut ReadBuf) -> usize {
        let data = self.read(target.remaining());
        target.put_slice(data);
        data.len()
    }

    /// Returns how many bytes remain in the buffer to be read
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.cursor
    }

    pub fn has_remaining(&self) -> bool {
        self.cursor < self.buf.len()
    }
}
