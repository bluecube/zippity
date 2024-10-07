#![cfg(feature = "tokio-file")]

use assert2::assert;
use std::{
    io::{SeekFrom, Write},
    pin::pin,
};
use tempfile::TempDir;
use test_strategy::proptest;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use zippity::proptest::TestEntryData;

use zippity::{Builder, TokioFileEntry};

#[proptest(async = "tokio")]
async fn read_slice_of_tokio_file_based_zip(
    content: TestEntryData,
    #[strategy(0f64..=1f64)] seek_pos_fraction: f64,
) {
    let tempdir = TempDir::new().unwrap();
    let mut builder_slices: Builder<&[u8]> = Builder::new();
    let mut builder_files: Builder<TokioFileEntry> = Builder::new();

    for (i, (name, value)) in content.0.iter().enumerate() {
        let path = tempdir.path().join(format!("{}", i));

        std::fs::File::create(path.clone())
            .unwrap()
            .write_all(value.as_ref())
            .unwrap();

        builder_slices
            .add_entry(name.clone(), value.as_ref())
            .await
            .unwrap();
        builder_files.add_entry(name.clone(), path).await.unwrap();
    }

    let mut zippity_slices = pin!(builder_slices.build());
    let mut zippity_files = pin!(builder_files.build());

    assert!(zippity_files.size() == zippity_slices.size());

    let seek_pos = (seek_pos_fraction * zippity_slices.size() as f64).floor() as u64;

    zippity_slices
        .seek(SeekFrom::Start(seek_pos))
        .await
        .unwrap();
    zippity_files.seek(SeekFrom::Start(seek_pos)).await.unwrap();

    let mut vec_slices = Vec::new();
    zippity_slices.read_to_end(&mut vec_slices).await.unwrap();
    let mut vec_files = Vec::new();
    zippity_files.read_to_end(&mut vec_files).await.unwrap();

    assert!(vec_files == vec_slices);
}
