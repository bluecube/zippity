use std::{collections::HashMap, pin::pin};

use test_strategy::proptest;
use tokio::io::AsyncReadExt;
use zip::ZipArchive;
use zippity::Builder;

#[tokio::test]
async fn empty_archive() {
    let mut zippity = pin!(Builder::<()>::new().build().unwrap());
    let size = zippity.size();

    let mut buf = Vec::new();
    zippity.read_to_end(&mut buf).await.unwrap();

    assert!(size == (buf.len() as u64));

    let unpacked = ZipArchive::new(std::io::Cursor::new(buf)).expect("Should be a valid zip");
    assert!(unpacked.is_empty());
}

#[tokio::test]
async fn empty_entry_name() {
    let mut builder: Builder<()> = Builder::new();

    builder.add_entry(String::new(), ()).await.unwrap();

    let mut zippity = pin!(builder.build().unwrap());
    let mut buf = Vec::new();
    zippity.read_to_end(&mut buf).await.unwrap();

    let mut unpacked = ZipArchive::new(std::io::Cursor::new(buf)).expect("Should be a valid zip");
    assert!(unpacked.len() == 1);

    let mut zipfile = unpacked.by_index(0).unwrap();
    let name = std::str::from_utf8(zipfile.name_raw()).unwrap().to_string();
    assert!(name.is_empty());
    let mut file_content = Vec::new();
    use std::io::Read;
    zipfile.read_to_end(&mut file_content).unwrap();
    assert!(file_content.is_empty());
}

#[tokio::test]
async fn archive_with_single_file() {
    let mut builder: Builder<&[u8]> = Builder::new();

    builder
        .add_entry("Foo".to_owned(), b"bar!".as_slice())
        .await
        .unwrap();

    let mut zippity = pin!(builder.build().unwrap());
    let size = zippity.size();

    let mut buf = Vec::new();
    zippity.read_to_end(&mut buf).await.unwrap();

    assert!(size == (buf.len() as u64));

    let mut unpacked = ZipArchive::new(std::io::Cursor::new(buf)).expect("Should be a valid zip");
    assert!(unpacked.len() == 1);

    let mut zipfile = unpacked.by_index(0).unwrap();
    let name = std::str::from_utf8(zipfile.name_raw()).unwrap().to_string();
    assert!(name == "Foo");
    let mut file_content = Vec::new();
    use std::io::Read;
    zipfile.read_to_end(&mut file_content).unwrap();
    assert!(file_content == b"bar!");
}

#[tokio::test]
async fn archive_with_single_empty_file() {
    let mut builder: Builder<&[u8]> = Builder::new();

    builder
        .add_entry("0".to_owned(), b"".as_slice())
        .await
        .unwrap();

    let mut zippity = pin!(builder.build().unwrap());
    let size = zippity.size();

    let mut buf = Vec::new();
    zippity.read_to_end(&mut buf).await.unwrap();

    assert!(size == (buf.len() as u64));

    let mut unpacked = ZipArchive::new(std::io::Cursor::new(buf)).expect("Should be a valid zip");
    assert!(unpacked.len() == 1);

    let mut zipfile = unpacked.by_index(0).unwrap();
    let name = std::str::from_utf8(zipfile.name_raw()).unwrap().to_string();
    assert!(name == "0");
    let mut file_content = Vec::new();
    use std::io::Read;
    zipfile.read_to_end(&mut file_content).unwrap();
    assert!(file_content == b"");
}

#[proptest(async = "tokio")]
async fn any_archive(reader_and_data: zippity::proptest::ReaderAndData) {
    let mut zippity = pin!(reader_and_data.reader);
    let content = reader_and_data.data;
    let size = zippity.size();

    let mut buf = Vec::new();
    zippity.read_to_end(&mut buf).await.unwrap();

    assert!(size == (buf.len() as u64));

    let mut unpacked = ZipArchive::new(std::io::Cursor::new(buf)).expect("Should be a valid zip");
    assert!(unpacked.len() == content.0.len());

    let mut unpacked_content = HashMap::new();
    for i in 0..unpacked.len() {
        dbg!(&i);
        let mut zipfile = unpacked.by_index(i).unwrap();
        let name = std::str::from_utf8(zipfile.name_raw()).unwrap().to_string();
        let mut file_content = Vec::new();
        use std::io::Read;
        zipfile.read_to_end(&mut file_content).unwrap();

        unpacked_content.insert(name, file_content.into());
    }
    assert!(unpacked_content == content.0);
}
