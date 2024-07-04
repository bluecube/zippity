use crate::{bytes::BytesStream, entry_data::EntryData, reader::Reader};
use actix_web::{
    body::{BodySize, BoxBody, MessageBody},
    http::{
        header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE},
        StatusCode,
    },
    HttpRequest, HttpResponse,
};
use bytes::Bytes;
use futures_core::Stream;
use http_range_header::parse_range_header;
use pin_project::{pin_project, pinned_drop};
use std::{
    future::Future,
    pin::Pin,
    task::{ready, Context, Poll},
};

impl<D: EntryData> Reader<D> {
    /// Wraps this reader into a struct that can be used as a return value from an Actix-web handler.
    pub fn into_responder(
        self,
    ) -> ActixWebAdapter<D, fn(Reader<D>) -> std::future::Ready<()>, std::future::Ready<()>> {
        ActixWebAdapter {
            inner: BytesStreamAndCallback {
                bytes_stream: self.into_bytes_stream(),
                callback: None,
            },
        }
    }

    /// Wraps this reader into a struct that can be used as a return value from an Actix-web handler.
    ///
    /// The callback specified is called when the wrapper is dropped, in a freshly spawned tokio task.
    /// The argument to the callback is a new reader equivalent to the original wrapped reader (see [Reader::take_pinned()]).
    pub fn into_responder_with_callback<Cb, Fut>(self, callback: Cb) -> ActixWebAdapter<D, Cb, Fut>
    where
        Cb: FnOnce(Reader<D>) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        ActixWebAdapter {
            inner: BytesStreamAndCallback {
                bytes_stream: self.into_bytes_stream(),
                callback: Some(callback),
            },
        }
    }
}

pub struct ActixWebAdapter<D, Cb, Fut>
where
    D: EntryData + 'static,
    Cb: FnOnce(Reader<D>) -> Fut + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    inner: BytesStreamAndCallback<D, Cb, Fut>,
}

impl<D, Cb, Fut> actix_web::Responder for ActixWebAdapter<D, Cb, Fut>
where
    D: EntryData + 'static,
    Cb: FnOnce(Reader<D>) -> Fut + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    type Body = BoxBody;

    fn respond_to(mut self, req: &HttpRequest) -> HttpResponse<Self::Body> {
        let mut response = HttpResponse::Ok();
        let size = self.inner.bytes_stream.reader_ref().size();

        response.insert_header((CONTENT_TYPE, "application/x-zip"));
        response.insert_header((ACCEPT_RANGES, "bytes"));
        // TODO: Content-disposition
        // TODO: Etag?
        // TODO: If-Range

        let (offset, length) = if let Some(ranges_header) = req.headers().get(RANGE) {
            let Some(parsed) = ranges_header
                .to_str()
                .ok()
                .and_then(|ranges_str| parse_range_header(ranges_str).ok())
            else {
                return response.status(StatusCode::BAD_REQUEST).finish();
            };
            let Some(validated) = parsed.validate(size).ok() else {
                response.insert_header((CONTENT_RANGE, format!("bytes */{}", size)));
                return response.status(StatusCode::RANGE_NOT_SATISFIABLE).finish();
            };
            let range = validated
                .first()
                .expect("Range header must be nonempty, this is verified by the parser");

            response.insert_header((
                CONTENT_RANGE,
                format!("bytes {}-{}/{}", *range.start(), *range.end(), size),
            ));
            (*range.start(), *range.end() - *range.start() + 1)
        } else {
            (0, size)
        };

        response.insert_header((CONTENT_LENGTH, length));

        if offset != 0 || length != size {
            response.status(StatusCode::PARTIAL_CONTENT);
        }

        self.inner
            .bytes_stream
            .reader_mut()
            .seek_from_start_mut(offset);

        response.body(Body::new(self.inner, length))
    }
}

#[pin_project(PinnedDrop)]
struct BytesStreamAndCallback<D, Cb, Fut>
where
    D: EntryData + 'static,
    Cb: FnOnce(Reader<D>) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    #[pin]
    bytes_stream: BytesStream<D>,
    callback: Option<Cb>,
}

#[pinned_drop]
impl<D, Cb, Fut> PinnedDrop for BytesStreamAndCallback<D, Cb, Fut>
where
    D: EntryData + 'static,
    Cb: FnOnce(Reader<D>) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    fn drop(self: Pin<&mut Self>) {
        let projected = self.project();
        let Some(callback) = projected.callback.take() else {
            return;
        };
        let extracted = projected.bytes_stream.reader_pin_mut().take_pinned();
        let fut = (callback)(extracted);
        tokio::spawn(fut);
    }
}

#[pin_project]
pub struct Body<D, Cb, Fut>
where
    D: EntryData + 'static,
    Cb: FnOnce(Reader<D>) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    #[pin]
    inner: BytesStreamAndCallback<D, Cb, Fut>,
    size: u64,
    remaining: u64,
}

impl<D, Cb, Fut> Body<D, Cb, Fut>
where
    D: EntryData + 'static,
    Cb: FnOnce(Reader<D>) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    fn new(inner: BytesStreamAndCallback<D, Cb, Fut>, size: u64) -> Self {
        Body {
            inner,
            size,
            remaining: size,
        }
    }
}

impl<D, Cb, Fut> MessageBody for Body<D, Cb, Fut>
where
    D: EntryData + 'static,
    Cb: FnOnce(Reader<D>) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    type Error = std::io::Error;

    fn size(&self) -> BodySize {
        BodySize::Sized(self.size)
    }

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Bytes, Self::Error>>> {
        let projected = self.project();

        if *projected.remaining == 0 {
            return Poll::Ready(None);
        }

        let bytes_stream = projected.inner.project().bytes_stream;
        let block = ready!(bytes_stream.poll_next(cx)).expect("The total number of bytes written is limited, we should never reach the actual end of the stream")?;

        if (block.len() as u64) > *projected.remaining {
            let limited_len = *projected.remaining as usize; // Just checked that remaining is small
            *projected.remaining = 0;
            Poll::Ready(Some(Ok(block.slice(0..limited_len))))
        } else {
            *projected.remaining -= block.len() as u64;
            Poll::Ready(Some(Ok(block)))
        }
    }
}

#[cfg(test)]
mod test {
    use actix_web::{body::MessageBody as _, http::header::RANGE, test, Responder};
    use assert2::assert;
    use bytes::Bytes;
    use futures_util::{stream::poll_fn, StreamExt};
    use std::pin::pin;
    use test_strategy::proptest;
    use tokio::sync::oneshot;

    use crate::{Builder, BytesStream, Reader};

    use super::{Body, BytesStreamAndCallback};

    #[tokio::test]
    // Test the headers are set as expected when requesting the zip file without range header.
    async fn default_response_headers() {
        let reader = Builder::<&[u8]>::new().build().await.unwrap();
        let size = reader.size();

        let responder = reader.into_responder();

        let req = test::TestRequest::get().to_http_request();
        let response = responder.respond_to(&req);
        let headers = response.headers();

        assert!(response.status().as_u16() == 200);
        let accept_ranges = headers
            .get("Accept-Ranges")
            .expect("Response must contain Accept-Ranges");
        assert!(accept_ranges == "bytes");
        let content_length = headers
            .get("Content-Length")
            .expect("Response must contain Content-Length");
        assert!(content_length == format!("{}", size).as_str());
        assert!(headers.get("Content-Range").is_none())
    }

    // Test the headers are set as expected when requesting the zip file with range header.
    #[tokio::test]
    async fn range_response_headers() {
        let reader = Builder::<&[u8]>::new().build().await.unwrap();
        let size = reader.size();

        let responder = reader.into_responder();

        let req = test::TestRequest::get()
            .insert_header((RANGE, "bytes=0-5"))
            .to_http_request();
        let response = responder.respond_to(&req);
        let headers = response.headers();

        assert!(response.status().as_u16() == 206);
        let accept_ranges = headers
            .get("Accept-Ranges")
            .expect("Response must contain Accept-Ranges");
        assert!(accept_ranges == "bytes");
        let content_length = headers
            .get("Content-Length")
            .expect("Response must contain Content-Length");
        assert!(content_length == "6");
        let content_range = headers
            .get("Content-Range")
            .expect("Response must contain Content-Range");
        assert!(content_range == format!("bytes 0-5/{}", size).as_str());
    }

    /// Tests that reader_and_callback calls the callback on drop
    #[proptest(async = "tokio")]
    async fn reader_and_callback_spawns_callback(reader: Reader<Bytes>) {
        let size = reader.size();
        let (tx1, rx1) = oneshot::channel();
        let (tx2, rx2) = oneshot::channel();

        async fn callback(
            remaining_reader: Reader<Bytes>,
            rx1: oneshot::Receiver<()>,
            tx2: oneshot::Sender<u64>,
        ) {
            rx1.await.unwrap();
            tx2.send(remaining_reader.size()).unwrap();
        }

        let bsac = BytesStreamAndCallback {
            bytes_stream: BytesStream::new(reader),
            callback: Some(|remaining_reader: Reader<Bytes>| callback(remaining_reader, rx1, tx2)),
        };

        drop(bsac);

        tx1.send(()).unwrap();
        let remaining_reader_size = rx2.await.unwrap();
        assert!(remaining_reader_size == size);
    }

    #[proptest(async = "tokio")]
    async fn body_size_limiting(reader: Reader<Bytes>, #[strategy(0f64..=1f64)] size_f: f64) {
        let read_length = (reader.size() as f64 * size_f).floor() as u64;

        let mut body = pin!(Body::new(reader.into_responder().inner, read_length));

        let data = poll_fn(|cx| body.as_mut().poll_next(cx))
            .fold(Vec::new(), |mut accumulator, block| async {
                accumulator.extend_from_slice(block.unwrap().as_ref());
                accumulator
            })
            .await;

        assert!(data.len() as u64 == read_length);
    }
}
