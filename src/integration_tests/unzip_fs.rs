use std::{
    collections::HashMap,
    fs,
    io::{self, Read},
    path::Path,
};

use crate::Builder;
use crate::test_util::{build_and_open, prepare_test_dir};
use assert2::assert;
use proptest::prelude::{Arbitrary, BoxedStrategy, Strategy};
use tempfile::TempDir;
use test_strategy::proptest;

/// Path entry, for generating arbitrary archive content
#[derive(Debug, Clone)]
enum Entry {
    File(Vec<u8>),
    Dir(HashMap<String, Entry>),
}

impl Arbitrary for Entry {
    type Parameters = (u32, u32, u32);
    type Strategy = BoxedStrategy<Entry>;

    fn arbitrary_with(args: Self::Parameters) -> Self::Strategy {
        proptest::collection::vec(proptest::bits::u8::ANY, 0..10)
            .prop_map(Entry::File)
            .prop_recursive(
                args.0,     // Max level count
                args.1,     // Target entry count
                args.2 / 2, // Expected branch size
                move |entry| {
                    proptest::collection::hash_map("[a-z]+", entry, 0..(args.2 as usize))
                        .prop_map(Entry::Dir)
                },
            )
            .boxed()
    }
}

impl Entry {
    fn make_files(&self, target: &Path) -> io::Result<()> {
        match self {
            Entry::File(bytes) => fs::write(target, bytes),
            Entry::Dir(entries) => {
                fs::create_dir(target)?;
                for (name, entry) in entries {
                    let mut p = target.to_owned();
                    p.push(name);
                    entry.make_files(&p)?;
                }

                Ok(())
            }
        }
    }

    fn make_expected_content(&self, mut name: String, out: &mut HashMap<String, Vec<u8>>) {
        match self {
            Entry::File(bytes) => assert!(out.insert(name, bytes.clone()).is_none()),
            Entry::Dir(entries) => {
                name += "/";
                assert!(out.insert(name.clone(), Vec::new()).is_none());
                for (entry_name, entry) in entries {
                    let entry_name = name.clone() + entry_name;
                    entry.make_expected_content(entry_name, out);
                }
            }
        }
    }
}

/// Tests zipping a temporary directory with arbitrary regular files and directories using `Builder::add_directory_recursive`.
#[proptest(async = "tokio")]
async fn any_archive_filesystem(#[any((8, 64, 16))] data: Entry) {
    let tempdir = TempDir::new().unwrap();
    let entry_path = {
        let mut p = tempdir.path().to_owned();
        p.push("x");
        p
    };

    let mut expected_content = HashMap::new();
    data.make_expected_content("x".to_owned(), &mut expected_content);

    data.make_files(&entry_path).unwrap();
    let mut builder = Builder::new();
    builder
        .add_directory_recursive(tempdir.path().to_owned(), None)
        .await
        .unwrap();
    let mut unpacked = build_and_open(builder).await;

    let mut unpacked_content = HashMap::new();
    for i in 0..unpacked.len() {
        let mut zipfile = unpacked.by_index(i).unwrap();
        let name = std::str::from_utf8(zipfile.name_raw()).unwrap().to_string();
        let mut file_content = Vec::new();
        use std::io::Read;
        zipfile.read_to_end(&mut file_content).unwrap();

        unpacked_content.insert(name, file_content);
    }

    dbg!(&unpacked_content);
    dbg!(&expected_content);
    assert!(unpacked_content == expected_content);
}

/// Tests zipping a temporary directory with arbitrary regular files and directories using `Builder::add_directory_recursive`.
#[cfg(unix)]
#[tokio::test]
async fn filesystem_file_dir_symlink() {
    let tempdir = prepare_test_dir();

    let mut builder = Builder::new();
    builder
        .add_directory_recursive(tempdir.path().to_owned(), None)
        .await
        .unwrap();
    let mut unpacked = build_and_open(builder).await;

    let unpacked_dir = unpacked
        .by_name("dir/")
        .expect("Directory must have an entry");
    assert!(unpacked_dir.size() == 0);
    assert!(unpacked_dir.unix_mode().expect("Must have unix mode") == 0o40755);
    drop(unpacked_dir);

    let mut unpacked_file = unpacked
        .by_name("dir/file")
        .expect("file must have an entry");
    let mut s = String::new();
    unpacked_file.read_to_string(&mut s).unwrap();
    assert!(s == "Hello world");
    assert!(unpacked_file.unix_mode().expect("Must have unix mode") == 0o100644);
    drop(unpacked_file);

    let mut unpacked_link1 = unpacked.by_name("link1").expect("link must have an entry");
    let mut s = String::new();
    unpacked_link1.read_to_string(&mut s).unwrap();
    assert!(s == "dir/file");
    assert!(unpacked_link1.unix_mode().expect("Must have unix mode") == 0o120777);
    drop(unpacked_link1);

    let mut unpacked_link2 = unpacked.by_name("link2").expect("link must have an entry");
    let mut s = String::new();
    unpacked_link2.read_to_string(&mut s).unwrap();
    assert!(s == "/foo/bar");
    assert!(unpacked_link2.unix_mode().expect("Must have unix mode") == 0o120777);
    drop(unpacked_link2);
}
