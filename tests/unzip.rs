use std::{collections::HashSet, pin::pin};

use assert2::assert;
use bytes::Bytes;
use indexmap::IndexMap;
use test_strategy::proptest;
use tokio::io::AsyncReadExt;
use zip::ZipArchive;
use zippity::{proptest::TestEntryData, Builder, EntryData};

#[tokio::test]
async fn empty_archive() {
    let unpacked = build_and_open(Builder::<()>::new()).await;
    assert!(unpacked.is_empty());
}

#[tokio::test]
async fn empty_entry_name() {
    let mut builder: Builder<()> = Builder::new();
    builder.add_entry(String::new(), ()).unwrap();

    let mut unpacked = build_and_open(builder).await;
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
        .unwrap();

    let mut unpacked = build_and_open(builder).await;
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

    builder.add_entry("0".to_owned(), b"".as_slice()).unwrap();

    let mut unpacked = build_and_open(builder).await;
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
async fn any_archive(content: TestEntryData) {
    let builder = content.clone().into();
    let mut unpacked = build_and_open(builder).await;
    assert!(unpacked.len() == content.0.len());

    let mut unpacked_content: IndexMap<String, Bytes> = IndexMap::new();
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

#[proptest(async = "tokio")]
async fn entry_ordering(entry_names: HashSet<String>) {
    let entry_names: Vec<_> = entry_names.into_iter().collect(); // Fix the order of the input

    let mut builder = Builder::<()>::new();

    for name in entry_names.iter() {
        builder.add_entry(name.clone(), ()).unwrap();
    }

    let mut unpacked = build_and_open(builder).await;

    let unpacked_entries: Vec<_> = (0..unpacked.len())
        .map(|i| {
            let zipfile = unpacked.by_index(i).unwrap();
            std::str::from_utf8(zipfile.name_raw()).unwrap().to_string()
        })
        .collect();

    assert!(unpacked_entries == entry_names);
}

#[proptest(async = "tokio")]
async fn file_modification_time(
    #[strategy(1980..=2107)] year: i32,
    #[strategy(1u32..=12u32)] month: u32,
    #[strategy(1u32..=31u32)] day: u32,
    #[strategy(0u32..=23u32)] hour: u32,
    #[strategy(0u32..=59u32)] minute: u32,
    #[strategy(0u32..=59u32)] second: u32,
) {
    let mut builder: Builder<()> = Builder::new();

    builder
        .add_entry("X".into(), ())
        .unwrap()
        .datetime_fields(year, month, day, hour, minute, second)
        .unwrap();

    let mut unpacked = build_and_open(builder).await;

    assert!(unpacked.len() == 1);

    let unpacked_timestamp = unpacked.by_index(0).unwrap().last_modified();

    assert!(unpacked_timestamp.year() == year as u16);
    assert!(unpacked_timestamp.month() == month as u8);
    assert!(unpacked_timestamp.day() == day as u8);
    assert!(unpacked_timestamp.hour() == hour as u8);
    assert!(unpacked_timestamp.minute() == minute as u8);
    assert!(unpacked_timestamp.second() == (second & !1) as u8);
}

#[tokio::test]
async fn regular_file_default_permissions() {
    let mut builder: Builder<()> = Builder::new();

    builder.add_entry("X".into(), ()).unwrap();

    let mut unpacked = build_and_open(builder).await;

    assert!(unpacked.len() == 1);

    let permissions = unpacked.by_index(0).unwrap().unix_mode().unwrap();

    assert!(permissions == 0o100644);
}

#[tokio::test]
async fn directory_default_permissions() {
    let mut builder: Builder<()> = Builder::new();

    builder.add_entry("X".into(), ()).unwrap().directory();

    let mut unpacked = build_and_open(builder).await;

    assert!(unpacked.len() == 1);

    let permissions = unpacked.by_index(0).unwrap().unix_mode().unwrap();

    assert!(permissions == 0o40755);
}

#[tokio::test]
async fn regular_file_override_permissions() {
    let mut builder: Builder<()> = Builder::new();

    builder
        .add_entry("X".into(), ())
        .unwrap()
        .unix_permissions(0o123);

    let mut unpacked = build_and_open(builder).await;

    assert!(unpacked.len() == 1);

    let permissions = unpacked.by_index(0).unwrap().unix_mode().unwrap();

    assert!(permissions == 0o100123);
}

#[tokio::test]
async fn directory_override_permissions() {
    let mut builder: Builder<()> = Builder::new();

    builder
        .add_entry("X".into(), ())
        .unwrap()
        .directory()
        .unix_permissions(0o123);

    let mut unpacked = build_and_open(builder).await;

    assert!(unpacked.len() == 1);

    let permissions = unpacked.by_index(0).unwrap().unix_mode().unwrap();

    assert!(permissions == 0o40123);
}

#[tokio::test]
async fn readonly_file_permissions() {
    let mut builder: Builder<()> = Builder::new();

    builder
        .add_entry("ro_file".into(), ())
        .unwrap()
        .readonly(true);

    let mut unpacked = build_and_open(builder).await;

    assert!(unpacked.len() == 1);

    let permissions = unpacked.by_index(0).unwrap().unix_mode().unwrap();

    // Expect readonly file => 0o100444
    assert!(permissions == 0o100444);
}

#[tokio::test]
async fn readonly_directory_permissions() {
    let mut builder: Builder<()> = Builder::new();

    builder
        .add_entry("ro_dir".into(), ())
        .unwrap()
        .directory()
        .readonly(true);

    let mut unpacked = build_and_open(builder).await;

    assert!(unpacked.len() == 1);

    let permissions = unpacked.by_index(0).unwrap().unix_mode().unwrap();

    // Expect readonly directory => 0o40555
    assert!(permissions == 0o40555);
}

#[tokio::test]
async fn symlink_permissions() {
    let mut builder: Builder<()> = Builder::new();

    builder
        .add_entry("X".into(), ())
        .unwrap()
        .symlink()
        .unix_permissions(0o123); // Will get overridden

    let mut unpacked = build_and_open(builder).await;

    assert!(unpacked.len() == 1);

    let permissions = unpacked.by_index(0).unwrap().unix_mode().unwrap();

    assert!(permissions == 0o120777);
}

async fn build_and_open<T: EntryData>(builder: Builder<T>) -> ZipArchive<std::io::Cursor<Vec<u8>>> {
    let mut zippity = pin!(builder.build());
    let size = zippity.size();

    let mut buf = Vec::new();
    zippity.read_to_end(&mut buf).await.unwrap();

    assert!(size == (buf.len() as u64));
    ZipArchive::new(std::io::Cursor::new(buf)).expect("Should be a valid zip")
}
