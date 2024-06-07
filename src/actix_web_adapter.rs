use crate::{
    entry_data::EntryData,
    reader::{Reader, READ_SIZE},
};
use actix_web::{
    body::{BodySize, BoxBody, MessageBody},
    http::{
        header::{ACCEPT_RANGES, CONTENT_RANGE, CONTENT_TYPE, RANGE},
        StatusCode,
    },
    HttpRequest, HttpResponse,
};
use assert2::assert;
use bytes::{Bytes, BytesMut};
use consume_on_drop::{Consume, ConsumeOnDrop};
use http_range_header::parse_range_header;
use pin_project::pin_project;
use std::{
    future::Future,
    pin::Pin,
    task::{ready, Context, Poll},
};
use tokio_util::io::poll_read_buf;

impl<D: EntryData> Reader<D> {
    pub fn into_responder(
        self,
    ) -> ActixWebAdapter<D, fn(Reader<D>) -> std::future::Ready<()>, std::future::Ready<()>> {
        self.into_responder_with_callback(noop_callback)
    }

    pub fn into_responder_with_callback<Cb, Fut>(self, callback: Cb) -> ActixWebAdapter<D, Cb, Fut>
    where
        Cb: FnOnce(Reader<D>) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        ActixWebAdapter {
            inner: ConsumeOnDrop::new(ReaderAndCallback {
                reader: self,
                callback,
            }),
        }
    }
}

pub struct ActixWebAdapter<D, Cb, Fut>
where
    D: EntryData + 'static,
    Cb: FnOnce(Reader<D>) -> Fut + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    inner: ConsumeOnDrop<ReaderAndCallback<D, Cb, Fut>>,
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
        let size = self.inner.reader.size();

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
                .expect("Range header must be nonemptyi, this is verified by the parser");
            (*range.start(), *range.end() - *range.start() + 1)
        } else {
            (0, size)
        };

        self.inner.reader.seek_from_start_mut(offset);

        response.body(Body::new(self.inner, length).boxed())
    }
}

#[pin_project]
struct ReaderAndCallback<D, Cb, Fut>
where
    D: EntryData + 'static,
    Cb: FnOnce(Reader<D>) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    #[pin]
    reader: Reader<D>,
    callback: Cb,
}

impl<D, Cb, Fut> Consume for ReaderAndCallback<D, Cb, Fut>
where
    D: EntryData + 'static,
    Cb: FnOnce(Reader<D>) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    fn consume(self) {
        let fut = (self.callback)(self.reader);
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
    inner: ConsumeOnDrop<ReaderAndCallback<D, Cb, Fut>>,
    size: u64,
    remaining: u64,
    buffer: BytesMut,
}

impl<D, Cb, Fut> Body<D, Cb, Fut>
where
    D: EntryData + 'static,
    Cb: FnOnce(Reader<D>) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    fn new(inner: ConsumeOnDrop<ReaderAndCallback<D, Cb, Fut>>, size: u64) -> Self {
        Body {
            inner,
            size,
            remaining: size,
            buffer: Self::get_buffer(size),
        }
    }

    fn get_buffer(remaining: u64) -> BytesMut {
        let size = if (READ_SIZE as u64) < remaining {
            READ_SIZE
        } else {
            remaining
                .try_into()
                .expect("We've just checked that remaining is less than a usize value")
        };
        BytesMut::with_capacity(size)
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

        assert!(projected.buffer.len() == 0);
        assert!(projected.buffer.capacity() > 0);

        let reader = projected.inner.as_pin_mut().project().reader;
        let n: u64 = ready!(poll_read_buf(reader, cx, projected.buffer))? as u64;

        assert!(n > 0, "We're keeping count in `remaining` for range requests, we should never reach the actual EOF.");
        assert!(
            n <= projected.remaining,
            "Read size should be limited when creating the buffer"
        );

        *projected.remaining -= n;

        let result =
            std::mem::replace(projected.buffer, Self::get_buffer(*projected.remaining)).freeze();

        Poll::Ready(Some(Ok(result)))
    }
}

pub fn noop_callback<T>(_: T) -> std::future::Ready<()> {
    std::future::ready(())
}

#[cfg(test)]
mod test {
    #[test]
    fn foo() {}
}
