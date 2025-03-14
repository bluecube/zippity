# Zippity

Library for asynchronously creating a ZIP file on the fly.

## Features

- Async, using tokio.
- ZIP is created on the fly, can be directly streamed somewhere, does not need to be stored in RAM or on disk
- Supports Zip64 (files > 4GB).
- File size is known in advance
- Output is driven from outside (implements `[tokio::io::AsyncRead]`)
- Allows seeking in the file (implements `[tokio::io::AsyncSeek]`)
- Supports files on the filesystem as entries
  - This part works, but is being reworked, see #6.
- Supports integration with Actix Web (`[Reader::into_responder()]`)
  - This part works, but is being reworked, see #5.

## Non-features
These are not currently planned:

- Compression: The zip only uses store method.
- Encryption
- Zip reading

## Example
```rust

use std::io::SeekFrom;
use tokio::io::{AsyncSeekExt, AsyncWriteExt, copy, sink};

tokio_test::block_on(async {

// Create the builder
let mut builder = zippity::Builder::<&[u8]>::new();

// Add data
builder.add_entry("Entry name".to_owned(), b"Entry data".as_slice()).unwrap();

// Build the reader object
// Note that this does not touch the data yet.
let mut zippity = builder.build();

// Getting file size is in O(1)
println!("Total zip file size will be {}B", zippity.size());

// Seek to last 10B
zippity.seek(SeekFrom::End(-10)).await.unwrap();

// Write to output (in this case a sink, throwing it away)
copy(&mut zippity, &mut sink()).await.unwrap();

})
```

## Current state
Consider this a beta version.
The library is functional, but there are two more API breaking changes planned and then a lot of polish needed before releasing 1.0.0.
See #1.

## Crate features

| Name | Description | Default |
| ---- | ----------- | ------- |
| `tokio-file` | Adds support for `TokioFileEntry` being used as a entry data through Tokio file. | yes |
| `bytes` | Implement `EntryData` for `bytes::Bytes`, and provide method `into_bytes_stream()` for `Reader`. | no |
| `actix-web` | Adds`actix_web::Reponder` implementation to `zippity::Reader` | no |
| `proptest` | Add module `zippity::proptest`, with strategies, and `proptest::arbitrary::Arbitrary` implementation for `Reader`. | no |
| `chrono` | Support for setting last modification date and time metadata using types from `chrono`. | no |
