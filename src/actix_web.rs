use crate::{bytes::BytesStream, entry_data::EntryData, reader::Reader};
use actix_web::{
    HttpRequest, HttpResponse,
    body::{BodySize, BoxBody, MessageBody},
    http::{
        StatusCode,
        header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE},
    },
};
use bytes::Bytes;
use futures_core::Stream;
use http_range_header::parse_range_header;
use pin_project::{pin_project, pinned_drop};
use std::{
    pin::Pin,
    task::{Context, Poll, ready},
};
use tokio::sync::oneshot;

impl<D: EntryData> Reader<D> {
    /// Wraps this reader into a struct that can be used as a return value from an Actix-web handler.
    ///
    /// See the docstring on [`ActixWebAdapter`] for info on response headers.
    pub fn into_responder(self) -> ActixWebAdapter<D> {
        ActixWebAdapter {
            inner: Inner {
                bytes_stream: self.into_bytes_stream(),
                drop_channel: None,
            },
        }
    }

    /// Wraps this reader into a struct that can be used as a return value from an Actix-web handler.
    ///
    /// Also returns a oneshot channel receiver that receives a Reader remaining state after the
    /// actix web adapter is dropped.
    /// This is useful to retrieve calculated CRCs after the zip file is served.
    /// See [`Reader::take_pinned()`], [`Reader::crc32s()`].
    ///
    /// See the docstring on [`ActixWebAdapter`] for info on response headers.
    pub fn into_responder_with_channel(self) -> (ActixWebAdapter<D>, oneshot::Receiver<Reader<D>>) {
        let (sender, receiver) = oneshot::channel();
        let adapter = ActixWebAdapter {
            inner: Inner {
                bytes_stream: self.into_bytes_stream(),
                drop_channel: Some(sender),
            },
        };

        (adapter, receiver)
    }
}

/// An adapter for using [`Reader`] as an `actix_web::Responder`.
///
/// The response sets the following headers:
/// - `Content-Type: application/zip`
/// - `Content-Length`
/// - `Accept-Ranges`
///
/// Default status code is `200 Ok`.
///
/// If header `Range` was set in input, the adapter tries to fullfill the request
/// and adds the `Content-Range` header.
/// Status can then also be :
/// - `400 Bad Request` (if range header had bad content)
/// - `416 Range Not Satisfiable` if range header had valid content but wrong for the content length
/// - `206 Partial Content` if only a partial response is produced based on the request
///
/// User should provide:
/// - `Content-Disposition`
/// - `Cache-Control`, `ETag`, `Last-Modified`
pub struct ActixWebAdapter<D: EntryData + 'static> {
    inner: Inner<D>,
}

impl<D: EntryData + 'static> actix_web::Responder for ActixWebAdapter<D> {
    type Body = BoxBody;

    fn respond_to(mut self, req: &HttpRequest) -> HttpResponse<Self::Body> {
        let mut response = HttpResponse::Ok();
        let size = self.inner.bytes_stream.reader_ref().size();

        response.insert_header((CONTENT_TYPE, "application/zip"));
        response.insert_header((ACCEPT_RANGES, "bytes"));

        let (offset, length) = if let Some(ranges_header) = req.headers().get(RANGE) {
            let Some(parsed) = ranges_header
                .to_str()
                .ok()
                .and_then(|ranges_str| parse_range_header(ranges_str).ok())
            else {
                return response.status(StatusCode::BAD_REQUEST).finish();
            };
            let Some(validated) = parsed.validate(size).ok() else {
                response.insert_header((CONTENT_RANGE, format!("bytes */{size}")));
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
struct Inner<D>
where
    D: EntryData + 'static,
{
    #[pin]
    bytes_stream: BytesStream<D>,
    drop_channel: Option<oneshot::Sender<Reader<D>>>,
}

#[pinned_drop]
impl<D> PinnedDrop for Inner<D>
where
    D: EntryData + 'static,
{
    fn drop(self: Pin<&mut Self>) {
        let projected = self.project();
        let Some(drop_channel) = projected.drop_channel.take() else {
            return;
        };

        // Extract the remaining state of the reader
        let extracted = projected.bytes_stream.reader_pin_mut().take_pinned();

        // Send the channel, ignoring if the other side already hung up
        let _ = drop_channel.send(extracted);
    }
}

#[pin_project]
pub struct Body<D: EntryData + 'static> {
    #[pin]
    inner: Inner<D>,
    size: u64,
    remaining: u64,
}

impl<D: EntryData + 'static> Body<D> {
    fn new(inner: Inner<D>, size: u64) -> Self {
        Body {
            inner,
            size,
            remaining: size,
        }
    }
}

impl<D: EntryData + 'static> MessageBody for Body<D> {
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

        if *projected.remaining < (block.len() as u64) {
            let limited_len =
                usize::try_from(*projected.remaining).expect("In this branch remaining is small");
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
    use actix_web::{Responder, body::MessageBody as _, http::header::RANGE};
    use assert2::assert;
    use futures_util::{StreamExt, stream::poll_fn};
    use std::pin::pin;
    use test_strategy::proptest;
    use tokio::sync::oneshot;

    use crate::{Builder, BytesStream, Reader, test_util::skip_length};

    use super::{Body, Inner};

    #[tokio::test]
    // Test the headers are set as expected when requesting the zip file without range header.
    async fn default_response_headers() {
        let reader = Builder::<&[u8]>::new().build();
        let size = reader.size();

        let responder = reader.into_responder();

        let req = actix_web::test::TestRequest::get().to_http_request();
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
        assert!(content_length == format!("{size}").as_str());
        assert!(headers.get("Content-Range").is_none());
    }

    // Test the headers are set as expected when requesting the zip file with range header.
    #[tokio::test]
    async fn range_response_headers() {
        let reader = Builder::<&[u8]>::new().build();
        let size = reader.size();

        let responder = reader.into_responder();

        let req = actix_web::test::TestRequest::get()
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
        assert!(content_range == format!("bytes 0-5/{size}").as_str());
    }

    /// Tests that reader_and_callback calls the callback on drop
    #[proptest(async = "tokio")]
    async fn reader_and_callback_spawns_callback(reader: Reader<Vec<u8>>) {
        let size = reader.size();
        let (tx, mut rx) = oneshot::channel();

        let bsac = Inner {
            bytes_stream: BytesStream::new(reader),
            drop_channel: Some(tx),
        };

        assert!(rx.try_recv().is_err());
        drop(bsac);
        let remaining_reader = rx.await.unwrap();

        assert!(remaining_reader.size() == size);
    }

    #[proptest(async = "tokio")]
    async fn body_size_limiting(reader: Reader<Vec<u8>>, #[strategy(0f64..=1f64)] size_f: f64) {
        let read_length = skip_length(usize::try_from(reader.size()).unwrap(), size_f);

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
