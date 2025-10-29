#![cfg(feature = "actix-web")]

use std::{io::SeekFrom, ops::Deref, pin::pin, sync::Arc};

use actix_test::TestServer;
use actix_web::{App, Responder, web};
use assert2::assert;
use async_http_range_reader::{AsyncHttpRangeReader, CheckSupportMethod};
use bytes::Bytes;
use test_strategy::proptest;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt},
    sync::Mutex,
};
use zippity::{
    Builder,
    proptest::{ReaderAndData, TestEntryData},
};

struct TestApp {
    entry_data: TestEntryData,
    callback_counter: Mutex<u32>,
}

async fn download_zip_no_callback(data: web::Data<TestApp>) -> impl Responder {
    let app = Arc::clone(data.deref());
    let builder: Builder<_> = app.entry_data.clone().into();
    let reader = builder.build();
    reader.into_responder()
}

async fn download_zip_yes_callback(data: web::Data<TestApp>) -> impl Responder {
    let app = Arc::clone(data.deref());
    let builder: Builder<_> = app.entry_data.clone().into();
    let reader = builder.build();

    async fn callback(app: Arc<TestApp>) {
        let mut locked = app.callback_counter.lock().await;
        *locked += 1;
    }

    reader.into_responder_with_callback(move |_reader| callback(app.clone()))
}

async fn read_to_vec(reader: impl AsyncRead) -> std::io::Result<Vec<u8>> {
    let mut buffer = Vec::new();
    let mut reader = pin!(reader);

    reader.read_to_end(&mut buffer).await?;

    Ok(buffer)
}

async fn prepare(
    data: ReaderAndData,
    use_callback: bool,
) -> (TestServer, web::Data<TestApp>, String, Vec<u8>) {
    let direct_read_result = read_to_vec(data.reader).await.unwrap();

    let app_data = web::Data::new(TestApp {
        entry_data: data.data,
        callback_counter: Default::default(),
    });
    let app_data_cloned = app_data.clone();
    let server = actix_test::start(move || {
        App::new()
            .app_data(app_data_cloned.clone())
            .route("/no_callback", web::get().to(download_zip_no_callback))
            .route("/yes_callback", web::get().to(download_zip_yes_callback))
    });

    let url = if use_callback {
        server.url("/yes_callback")
    } else {
        server.url("/no_callback")
    };

    (server, app_data, url, direct_read_result)
}

#[proptest(async = "tokio")]
async fn read_all(data: ReaderAndData, use_callback: bool) {
    let (_server, app_data, url, direct_read_result) = prepare(data, use_callback).await;

    let direct_read_result = Bytes::from(direct_read_result);

    let response = reqwest::Client::new().get(url).send().await.unwrap();
    let http_read_result = response.bytes().await.unwrap();

    assert!(http_read_result == direct_read_result);

    let callback_count = *app_data.callback_counter.lock().await;
    if use_callback {
        assert!(callback_count == 1);
    } else {
        assert!(callback_count == 0);
    }
}

#[proptest(async = "tokio")]
async fn read_block(
    data: ReaderAndData,
    use_callback: bool,
    #[strategy(0f64..1f64)] boundary1: f64,
    #[strategy(0f64..1f64)] boundary2: f64,
) {
    let (_server, app_data, url, direct_read_result) = prepare(data, use_callback).await;

    let mut http_reader = pin!(
        AsyncHttpRangeReader::new(
            reqwest::Client::new(),
            reqwest::Url::parse(&url).unwrap(),
            CheckSupportMethod::NegativeRangeRequest(1),
            Default::default(),
        )
        .await
        .unwrap()
        .0
    );

    let start = (boundary1.min(boundary2) * direct_read_result.len() as f64).floor() as usize;
    let end = (boundary1.max(boundary2) * direct_read_result.len() as f64).floor() as usize;

    http_reader
        .seek(SeekFrom::Start(start as u64))
        .await
        .unwrap();
    let mut http_read_result = vec![0; end - start + 1];
    http_reader
        .read_exact(http_read_result.as_mut())
        .await
        .unwrap();

    assert!(http_read_result == direct_read_result[start..=end]);

    let callback_count = *app_data.callback_counter.lock().await;
    if use_callback {
        assert!(callback_count > 0);
    } else {
        assert!(callback_count == 0);
    }
}
