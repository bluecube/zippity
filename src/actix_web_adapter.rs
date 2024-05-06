// use std::{
//     pin::Pin,
//     task::{ready, Context, Poll},
// };

use crate::{/*reader::READ_SIZE, */ EntryData, Reader};
use actix_web::{
    body::{/*BodySize, */ BoxBody, MessageBody, SizedStream},
    http::{
        header::{ACCEPT_RANGES, CONTENT_RANGE, CONTENT_TYPE, RANGE},
        StatusCode,
    },
    HttpRequest, HttpResponse, Responder,
};
// use bytes::{Bytes, BytesMut};
use http_range_header::parse_range_header;
// use pin_project::pin_project;
use tokio::io::AsyncReadExt;
use tokio_util::io::{/*poll_read_buf,*/ ReaderStream};

impl<D: EntryData<'static> + 'static> Reader<'static, D> {
    pub fn into_response(mut self, req: &HttpRequest) -> HttpResponse<BoxBody> {
        let mut res = HttpResponse::Ok();

        res.insert_header((CONTENT_TYPE, "application/x-zip"));
        res.insert_header((ACCEPT_RANGES, "bytes"));
        // TODO: Content-disposition
        // TODO: Etag?
        // TODO: If-Range

        let (offset, length) = if let Some(ranges_header) = req.headers().get(RANGE) {
            let Some(parsed) = ranges_header
                .to_str()
                .ok()
                .and_then(|ranges_str| parse_range_header(ranges_str).ok())
            else {
                return res.status(StatusCode::BAD_REQUEST).finish();
            };
            let Some(validated) = parsed.validate(self.size()).ok() else {
                res.insert_header((CONTENT_RANGE, format!("bytes */{}", self.size())));
                return res.status(StatusCode::RANGE_NOT_SATISFIABLE).finish();
            };
            let range = validated
                .first()
                .expect("Range header must be nonemptyi, this is verified by the parser");
            (*range.start(), *range.end() - *range.start() + 1)
        } else {
            (0, self.size())
        };

        self.seek_from_start_mut(offset);

        return res.body(SizedStream::new(length, ReaderStream::new(self.take(length))).boxed());
    }
}

impl<D: EntryData<'static> + 'static> Responder for Reader<'static, D> {
    type Body = BoxBody;

    fn respond_to(self, req: &HttpRequest) -> HttpResponse<Self::Body> {
        self.into_response(req)
    }
}

// #[pin_project]
// struct Body<D: EntryData> {
//     #[pin]
//     reader: Reader<D>,
//     buf: BytesMut,
//     size: u64,
//     remaining: u64,
// }

// impl<D: EntryData> MessageBody for Body<D> {
//     type Error = std::io::Error;

//     fn size(&self) -> BodySize {
//         return BodySize::Sized(self.size);
//     }

//     fn poll_next(
//         self: Pin<&mut Self>,
//         cx: &mut Context<'_>,
//     ) -> Poll<Option<Result<Bytes, Self::Error>>> {
//         let projected = self.project();

//         if *projected.remaining == 0 {
//             return Poll::Ready(None);
//         }

//         projected.buf.reserve(
//             (*projected.remaining)
//                 .min(READ_SIZE as u64)
//                 .try_into()
//                 .expect("Reserve size is limited by READ_SIZE and therefore fits into usize"),
//         );

//         let n = ready!(poll_read_buf(projected.reader, cx, projected.buf))?;
//         assert!(n > 0, "We're keeping count in `remaining` for range requests, we should never reach the actual EOF.");

//         if *projected.remaining < projected.buf.len() as u64 {
//             projected.buf.truncate(
//                 (*projected.remaining)
//                     .try_into()
//                     .expect("Just checked above that the `remaining` is small"),
//             );
//         }
//         *projected.remaining -= projected.buf.len() as u64;
//         Poll::Ready(Some(Ok(projected.buf.split().freeze())))
//     }
// }

#[cfg(test)]
mod test {
    #[test]
    fn foo() {}
}
