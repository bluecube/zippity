//! These are the integration tests for zippity.
//! These are here under src/ instead of in tests/, because we need to access
//! the module `test_util`.

#[cfg(feature = "actix-web")]
mod actix_web;
mod external;
mod unzip;
#[cfg(feature = "tokio-file")]
mod unzip_fs;
