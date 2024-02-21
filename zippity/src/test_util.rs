#![cfg(test)]

use assert2::assert;
use proptest::strategy::{Just, Strategy};
use std::io::Result;
use std::ops::Range;
use std::{future::Future, pin::Pin};
use tokio::io::{AsyncRead, AsyncReadExt};

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

pub fn unasync<Fut: Future>(fut: Fut) -> Fut::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(fut)
}
