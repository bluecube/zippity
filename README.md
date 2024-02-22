# Zippity

A library for creating a ZIP file on the fly. Currently work in progress.

## Features

- [x] Async, using tokio.
- [x] ZIP is created on the fly, can be directly streamed somewhere, does not need to be stored in RAM or on disk
- [x] Supports Zip64 (files > 4GB).
- [x] Simple API
- [ ] Allows seeking in the file
- [ ] File size is known in advance

## Non-features
- Compression: The zip only uses store method.
- Encryption
- Zip reading
