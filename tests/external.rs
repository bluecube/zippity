use assert2::assert;
use core::panic;
use std::{collections::HashMap, path::Path, pin::pin};
use tempfile::NamedTempFile;
use test_strategy::proptest;
use tokio::{fs::File, io, process::Command};
use zippity::{
    proptest::{ArbitraryTestEntryDataParams, TestEntryData},
    Reader,
};

/// Runs a given command on a zipfile created from the given test data.
/// Panics if the command fails, returns its stdout if it succeds.
async fn external_command_test<F: FnOnce(&Path) -> Command>(
    data: TestEntryData,
    command_builder: F,
) -> Vec<u8> {
    let mut zippity = pin!(Into::<Reader<_>>::into(data));

    let tempfile = NamedTempFile::new().unwrap();
    let (f, temp_path) = tempfile.into_parts();
    let mut f = File::from_std(f);

    io::copy(&mut zippity.as_mut(), &mut f).await.unwrap();
    f.sync_all().await.unwrap();
    drop(f);

    let unzip_output = command_builder(&temp_path)
        .output()
        .await
        .expect("Could not run command");

    if !unzip_output.status.success() {
        // temp_path.persist("/tmp/bad.zip").unwrap();
        panic!(
            "command failed\nstdout:\n{}\nstderr:\n{}",
            std::str::from_utf8(&unzip_output.stdout).unwrap(),
            std::str::from_utf8(&unzip_output.stderr).unwrap()
        );
    }

    unzip_output.stdout
}

/// Tests that a zip file works with `unzip -t` without error.
#[proptest(async = "tokio")]
async fn unzip_t(
    #[any(ArbitraryTestEntryDataParams {
        entry_name_pattern: ".{1,30}", // Unzip doesn't like empty entry names
            // Fails with  `mismatching "local" filename (foo), continuing with "central" filename version`
        count_range: 1..64, // Unzip reports empty zip as error.
        ..Default::default()
    })]
    data: TestEntryData,
) {
    external_command_test(data, |p| {
        let mut command = Command::new("unzip");
        command.arg("-t").arg(p.as_os_str());
        command
    })
    .await;
}

/// Checks that a zip file can be opened through python's `zipfile` module
/// and that entry names and sizes match.
#[proptest(async = "tokio")]
async fn python_zipfile(
    #[any(ArbitraryTestEntryDataParams {
        entry_name_pattern: "[^\0]{0,30}", // zipfile doesn't handle null bytes well
        ..Default::default()
    })]
    data: TestEntryData,
) {
    let json_data = external_command_test(data.clone(), |p| {
        let mut command = Command::new("python");
        command
            .arg("-c")
            .arg(
                "
import zipfile
import sys
import json
zf = zipfile.ZipFile(sys.argv[1])
output = {}
for info in zf.filelist:
    output[info.filename] = info.file_size
    with zf.open(info.filename, 'r') as f:
        while f.read(2**20):
            pass
print(json.dumps(output))
        ",
            )
            .arg(p.as_os_str());
        command
    })
    .await;
    let unzipped_summary: HashMap<String, usize> = serde_json::from_slice(&json_data).unwrap();

    dbg!(&unzipped_summary);

    assert!(unzipped_summary.len() == data.0.len());
    for (k, len) in unzipped_summary {
        let Some(expected_bytes) = data.0.get(&k) else {
            panic!("Key {:?} should exist in the unpacked summary", &k);
        };

        assert!(len == expected_bytes.len());
    }
}
