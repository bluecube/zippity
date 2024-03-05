#![cfg(test)]

use assert2::assert;
use pin_project::pin_project;
use proptest::strategy::{Just, Strategy};
use std::io::Result;
use std::ops::Range;
use std::task::Poll;
use std::{future::Future, pin::Pin};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::EntryData;

/// Returns a proptest strategy that minimizes to maximum read size
pub fn read_size_strategy() -> impl Strategy<Value = usize> {
    const MIN: usize = 1;
    const MAX: usize = 8192;
    (MIN..=MAX).prop_map(|v| (MAX + MIN - v))
}

pub fn nonempty_range_strategy(limit: usize) -> impl Strategy<Value = Range<usize>> {
    assert!(limit > 0);
    (1..limit + 1)
        .prop_flat_map(|upper| (0..upper, Just(upper)))
        .prop_map(|(lower, upper)| lower..upper)
}

/// Takes an async readable, collects all data to vec.
/// Size of each read can be specified
pub async fn read_to_vec(
    mut reader: Pin<&mut impl AsyncRead>,
    read_size: usize,
) -> Result<Vec<u8>> {
    let mut buffer = Vec::new();

    loop {
        let size_before = buffer.len();
        buffer.resize(size_before + read_size, 0);
        let write_slice = &mut buffer[size_before..];
        assert!(write_slice.len() == read_size);

        let size_read = reader.read(write_slice).await?;

        buffer.truncate(size_before + size_read);

        if size_read == 0 {
            return Ok(buffer);
        }
    }
}

/// Takes an async readable, goes through all its data discarding it,
/// returns the total number of bytes.
pub async fn measure_size(mut reader: Pin<&mut impl AsyncRead>) -> Result<u64> {
    let mut buffer = vec![0; 8192];
    let mut size = 0;

    loop {
        let read_size = reader.read(buffer.as_mut_slice()).await?;
        if read_size == 0 {
            return Ok(size);
        } else {
            size += read_size as u64;
        }
    }
}

/// Run a future in a new tokio runtime, block until it finishes and return its output
pub fn unasync<Fut: Future>(fut: Fut) -> Fut::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}

/// Async readable that returns zeros.
#[derive(Clone, Debug)]
#[pin_project]
pub struct ZerosReader {
    size: u64,
    remaining: u64,
}

impl ZerosReader {
    pub fn new(size: u64) -> Self {
        ZerosReader {
            size,
            remaining: size,
        }
    }
}

impl AsyncRead for ZerosReader {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.remaining > 0 {
            let n = self.remaining.min(buf.remaining() as u64);
            buf.initialize_unfilled_to(n as usize).fill(0);
            buf.advance(n as usize);
            *self.project().remaining -= n;
        }
        Poll::Ready(Ok(()))
    }
}

impl EntryData for ZerosReader {
    type Reader = Self;
    type ReaderFuture = std::future::Ready<Result<Self>>;

    fn size(&self) -> u64 {
        self.size
    }

    fn get_reader(&self) -> Self::ReaderFuture {
        std::future::ready(Ok(self.clone()))
    }
}
