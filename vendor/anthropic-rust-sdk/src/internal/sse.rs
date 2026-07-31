//! Server-Sent Events 解码器，对齐上游 `src/core/streaming.ts`。

use crate::core::error::{AnthropicError, Error};
use bytes::Bytes;
use futures::Stream;
use std::pin::Pin;
use std::task::{Context, Poll};

/// SSE 原始事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSentEvent {
    pub event: Option<String>,
    pub data: String,
}

/// SSE 字节流解码器。
pub struct SseDecoder<S> {
    inner: S,
    buffer: String,
    finished: bool,
}

impl<S> SseDecoder<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            buffer: String::new(),
            finished: false,
        }
    }
}

impl<S> Stream for SseDecoder<S>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Unpin,
{
    type Item = Result<ServerSentEvent, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.finished {
            return Poll::Ready(None);
        }

        loop {
            if let Some(idx) = find_double_newline(&self.buffer) {
                let raw = self.buffer[..idx].to_string();
                self.buffer = self.buffer[idx..].trim_start_matches('\n').to_string();
                if let Some(sse) = parse_sse_chunk(&raw) {
                    return Poll::Ready(Some(Ok(sse)));
                }
                continue;
            }

            match Pin::new(&mut self.inner).poll_next(cx) {
                Poll::Ready(Some(Ok(chunk))) => {
                    self.buffer.push_str(&String::from_utf8_lossy(&chunk));
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(Error::Connection(
                        crate::core::error::ConnectionError {
                            message: e.to_string(),
                            source: Some(Box::new(e)),
                        },
                    ))));
                }
                Poll::Ready(None) => {
                    self.finished = true;
                    if !self.buffer.trim().is_empty() {
                        if let Some(sse) = parse_sse_chunk(self.buffer.trim()) {
                            self.buffer.clear();
                            return Poll::Ready(Some(Ok(sse)));
                        }
                    }
                    return Poll::Ready(None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// 判断 SSE 事件名是否属于 Messages 流式事件。
pub fn is_message_stream_event(event: Option<&str>) -> bool {
    matches!(
        event,
        Some(
            "message_start"
                | "message_delta"
                | "message_stop"
                | "content_block_start"
                | "content_block_delta"
                | "content_block_stop"
                | "error"
                | "ping"
                | "event_start"
                | "event_delta"
                | "system.message"
        )
    ) || event.is_none()
}

/// 将 SSE JSON 数据解析为类型 `T`。
pub fn parse_sse_json<T: serde::de::DeserializeOwned>(data: &str) -> Result<T, Error> {
    serde_json::from_str(data).map_err(|e| {
        Error::Anthropic(AnthropicError(format!(
            "Could not parse SSE message into JSON: {e}; data={data}"
        )))
    })
}

fn find_double_newline(s: &str) -> Option<usize> {
    s.find("\n\n").or_else(|| s.find("\r\n\r\n"))
}

fn parse_sse_chunk(raw: &str) -> Option<ServerSentEvent> {
    let mut event = None;
    let mut data_lines = Vec::new();

    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("event:") {
            event = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim_start());
        }
    }

    if data_lines.is_empty() && event.is_none() {
        return None;
    }

    Some(ServerSentEvent {
        event,
        data: data_lines.join("\n"),
    })
}
