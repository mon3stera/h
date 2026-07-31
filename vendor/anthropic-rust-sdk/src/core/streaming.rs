//! 流式响应封装。

use crate::core::error::Error;
use crate::internal::sse::{is_message_stream_event, parse_sse_json, ServerSentEvent, SseDecoder};
use bytes::Bytes;
use futures::Stream;
use std::pin::Pin;
use std::task::{Context, Poll};

/// 字节流类型别名。
pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>;

/// 原始 SSE 事件流。
pub struct RawEventStream {
    decoder: SseDecoder<ByteStream>,
}

impl RawEventStream {
    pub fn new(inner: ByteStream) -> Self {
        Self {
            decoder: SseDecoder::new(inner),
        }
    }
}

impl Stream for RawEventStream {
    type Item = Result<ServerSentEvent, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.decoder).poll_next(cx)
    }
}

/// 解析后的 JSON 事件流。
pub struct EventStream<T> {
    decoder: SseDecoder<ByteStream>,
    _marker: std::marker::PhantomData<T>,
}

impl<T> EventStream<T> {
    pub fn new(inner: ByteStream) -> Self {
        Self {
            decoder: SseDecoder::new(inner),
            _marker: std::marker::PhantomData,
        }
    }
}

impl<T> Stream for EventStream<T>
where
    T: serde::de::DeserializeOwned + Unpin,
{
    type Item = Result<T, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            match Pin::new(&mut self.decoder).poll_next(cx) {
                Poll::Ready(Some(Ok(sse))) => {
                    if !is_message_stream_event(sse.event.as_deref()) || sse.data.is_empty() {
                        continue;
                    }
                    return Poll::Ready(Some(parse_sse_json(&sse.data)));
                }
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Some(Err(e))),
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}
