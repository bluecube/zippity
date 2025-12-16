use std::{
    io::Result,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::{Bytes, BytesMut};
use futures_core::Stream;
use pin_project::pin_project;
use tokio_util::io::poll_read_buf;

use crate::{Reader, entry_data::EntryData, reader::READ_SIZE};

#[derive(Clone, Debug)]
#[pin_project]
pub struct BytesStream<D: EntryData> {
    #[pin]
    reader: Reader<D>,
    buffer: BytesMut,
}

// TODO: Implement also std::async_iter::AsyncIterator, once it stablises
impl<D: EntryData> Stream for BytesStream<D> {
    type Item = Result<Bytes>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let projected = self.project();

        assert!(projected.buffer.is_empty());
        assert!(projected.buffer.capacity() > 0);

        let n: u64 =
            std::task::ready!(poll_read_buf(projected.reader, cx, projected.buffer))? as u64;

        if n == 0 {
            return Poll::Ready(None);
        }

        // TODO: Fancier buffer allocation? Try to reuse the previously allocated buffer using try_mut? MEASURE!
        let result =
            std::mem::replace(projected.buffer, BytesMut::with_capacity(READ_SIZE)).freeze();

        Poll::Ready(Some(Ok(result)))
    }
}

impl<D: EntryData> BytesStream<D> {
    pub fn new(reader: Reader<D>) -> Self {
        BytesStream {
            reader,
            buffer: BytesMut::with_capacity(READ_SIZE),
        }
    }

    pub fn into_reader(self) -> Reader<D> {
        self.reader
    }

    pub fn reader_ref(&self) -> &Reader<D> {
        &self.reader
    }

    pub fn reader_mut(&mut self) -> &mut Reader<D> {
        &mut self.reader
    }

    pub fn reader_pin_mut(self: Pin<&mut Self>) -> Pin<&mut Reader<D>> {
        self.project().reader
    }
}

impl<D: EntryData> From<Reader<D>> for BytesStream<D> {
    fn from(value: Reader<D>) -> Self {
        BytesStream::new(value)
    }
}

impl<D: EntryData> Reader<D> {
    pub fn into_bytes_stream(self) -> BytesStream<D> {
        BytesStream::new(self)
    }
}

#[cfg(test)]
mod test {
    use std::pin::pin;

    use bytes::Bytes;
    use futures_util::StreamExt;
    use test_strategy::proptest;

    use crate::{
        builder::Builder,
        test_util::{read_size_strategy, read_to_vec, test_entry_data::TestEntryData},
    };

    /// Test that the zip file comes out identical between &[u8] and Bytes
    #[proptest(async = "tokio")]
    async fn bytes_and_u8_slice_give_identical_results(
        content: TestEntryData,
        #[strategy(read_size_strategy())] read_size: usize,
    ) {
        let mut builder_slice: Builder<&[u8]> = Builder::new();
        let mut builder_bytes: Builder<Bytes> = Builder::new();
        for (name, value) in &content.0 {
            builder_slice
                .add_entry(name.clone(), value.as_ref())
                .unwrap();
            builder_bytes
                .add_entry(name.clone(), value.clone())
                .unwrap();
        }

        let reader_slice = pin!(builder_slice.build());
        let reader_bytes = pin!(builder_bytes.build());

        let data_slice = read_to_vec(reader_slice, read_size).await.unwrap();
        let data_bytes = read_to_vec(reader_bytes, read_size).await.unwrap();

        assert!(data_bytes == data_slice);
    }

    #[proptest(async = "tokio")]
    async fn bytes_stream_provides_correct_data(
        content: TestEntryData,
        #[strategy(read_size_strategy())] read_size: usize,
    ) {
        let mut builder: Builder<Bytes> = Builder::new();
        for (name, value) in &content.0 {
            builder.add_entry(name.clone(), value.clone()).unwrap();
        }

        let bytes_stream = pin!(builder.clone().build().into_bytes_stream());
        let reader = pin!(builder.clone().build());

        let data_reader = read_to_vec(reader, read_size).await.unwrap();
        let data_stream = bytes_stream
            .fold(
                Vec::with_capacity(data_reader.len()),
                |mut accumulator, b| {
                    accumulator.extend_from_slice(b.unwrap().as_ref());
                    std::future::ready(accumulator)
                },
            )
            .await;

        assert!(data_reader == data_stream);
    }
}
