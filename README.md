# Zippity

A library for creating a ZIP file on the fly. Currently work in progress.

## Features

- [x] Async, using tokio.
- [x] ZIP is created on the fly, can be directly streamed somewhere, does not need to be stored in RAM or on disk
- [x] Supports Zip64 (files > 4GB).
- [x] File size is known in advance
- [x] Output is driven from outside (implements `tokio::io::AsyncRead`)
- [ ] Allows seeking in the file (implements `tokio::io::AsyncSeek`)
- [ ] Supports files on the filesystem as entries.
- [ ] Supports integration with Actix Web (implements `actix_web::Responder`) behind feature flag

## Non-features
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
builder.add_entry("Entry name".to_owned(), b"Entry data".as_slice());

// Build the reader object
// Note that this does not touch the data yet.
let mut zippity = builder.build();

// Getting file size is in O(1)
println!("Total zip file size will be {}B", zippity.size());

// Write to output (in this case a sink, throwing it away)
copy(&mut zippity, &mut sink()).await.unwrap();

})

```
