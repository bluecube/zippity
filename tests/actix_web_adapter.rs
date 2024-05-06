#![cfg(feature = "actix-web-adapter")]

use std::collections::HashMap;

use actix_web::{web, App, HttpServer, Responder};
use async_http_range_reader;
use proptest::strategy::Strategy;
use tempfile::tempdir;
use test_strategy::proptest;
use tokio_test::block_on;

// This function is duplicated from private zippity::test_util::content_strategy ... oh well...
pub fn content_strategy() -> impl Strategy<Value = HashMap<String, Vec<u8>>> {
    proptest::collection::hash_map(
        ".*",
        proptest::collection::vec(proptest::bits::u8::ANY, 0..100),
        0..100,
    )
}

async fn make_test_zip(data: web::Data<HashMap<String, Vec<u8>>>) -> impl Responder {
    let mut builder = zippity::Builder::<&[u8]>::new();

    data.iter().for_each(|(name, value)| {
        builder.add_entry(name.clone(), value.as_ref()).unwrap();
    });

    builder.build()
}

#[proptest]
fn any_archive(#[strategy(content_strategy())] content: HashMap<String, Vec<u8>>) {
    let temp_dir = tempdir().unwrap();
    let socket_path = temp_dir.path().join("server.socket");

    block_on(async {
        tokio::spawn(
            HttpServer::new(|| {
                App::new()
                    .app_data(web::Data::new(content))
                    .route("/zip", web::get().to(make_test_zip))
            })
            .bind_uds(socket_path)
            .unwrap()
            .run(),
        );
    });
}
