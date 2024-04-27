use std::{collections::HashMap, pin::pin};

use proptest::strategy::Strategy;
use test_strategy::proptest;
use tokio::io::AsyncReadExt;
use tokio_test::block_on;
use zip::ZipArchive;
use zippity::Builder;

// This function is duplicated from private zippity::test_util::content_strategy ... oh well...
pub fn content_strategy() -> impl Strategy<Value = HashMap<String, Vec<u8>>> {
    proptest::collection::hash_map(
        ".*",
        proptest::collection::vec(proptest::bits::u8::ANY, 0..100),
        0..100,
    )
}

#[test]
fn empty_archive() {
    let mut zippity = pin!(Builder::<()>::new().build());
    let size = zippity.size();

    let mut buf = Vec::new();
    block_on(zippity.read_to_end(&mut buf)).unwrap();

    assert!(size == (buf.len() as u64));

    let unpacked = ZipArchive::new(std::io::Cursor::new(buf)).expect("Should be a valid zip");
    assert!(unpacked.is_empty());
}

#[test]
fn empty_entry_name() {
    let mut builder: Builder<()> = Builder::new();

    builder.add_entry(String::new(), ()).unwrap();

    let mut zippity = pin!(builder.build());
    let mut buf = Vec::new();
    block_on(zippity.read_to_end(&mut buf)).unwrap();

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

#[test]
fn archive_with_single_file() {
    let mut builder: Builder<&[u8]> = Builder::new();

    builder
        .add_entry("Foo".to_owned(), b"bar!".as_slice())
        .unwrap();

    let mut zippity = pin!(builder.build());
    let size = zippity.size();

    let mut buf = Vec::new();
    block_on(zippity.read_to_end(&mut buf)).unwrap();

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

#[test]
fn archive_with_single_empty_file() {
    let mut builder: Builder<&[u8]> = Builder::new();

    builder.add_entry("0".to_owned(), b"".as_slice()).unwrap();

    let mut zippity = pin!(builder.build());
    let size = zippity.size();

    let mut buf = Vec::new();
    block_on(zippity.read_to_end(&mut buf)).unwrap();

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

#[proptest]
fn any_archive(#[strategy(content_strategy())] content: HashMap<String, Vec<u8>>) {
    let mut builder: Builder<&[u8]> = Builder::new();

    content.iter().for_each(|(name, value)| {
        builder.add_entry(name.clone(), value.as_ref()).unwrap();
    });

    let mut zippity = pin!(builder.build());
    let size = zippity.size();

    let mut buf = Vec::new();
    block_on(zippity.read_to_end(&mut buf)).unwrap();

    assert!(size == (buf.len() as u64));

    let mut unpacked = ZipArchive::new(std::io::Cursor::new(buf)).expect("Should be a valid zip");
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
    assert!(unpacked_content == content);
}
