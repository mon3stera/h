//! SSE 解析测试。

use anthropic_rust_sdk::core::streaming::EventStream;
use futures::{stream, StreamExt};

#[tokio::test]
async fn parses_message_stream_events() {
    let raw = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m1\"}}\n\n\
               event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n\
               event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";

    let stream = stream::iter(vec![Ok(bytes::Bytes::from(raw))]);
    let mut events = EventStream::<serde_json::Value>::new(stream.boxed());

    let first = events.next().await.unwrap().unwrap();
    assert_eq!(first["type"], "message_start");

    let second = events.next().await.unwrap().unwrap();
    assert_eq!(second["type"], "content_block_delta");

    let third = events.next().await.unwrap().unwrap();
    assert_eq!(third["type"], "message_stop");
}

#[tokio::test]
async fn passes_through_system_message_event() {
    let raw = "event: system.message\ndata: {\"type\":\"system.message\",\"message\":{\"role\":\"system\",\"content\":\"ctx\"}}\n\n";

    let stream = stream::iter(vec![Ok(bytes::Bytes::from(raw))]);
    let mut events = EventStream::<serde_json::Value>::new(stream.boxed());

    let event = events.next().await.unwrap().unwrap();
    assert_eq!(event["type"], "system.message");
    assert_eq!(event["message"]["role"], "system");
}

#[tokio::test]
async fn passes_through_error_event() {
    let raw = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";

    let stream = stream::iter(vec![Ok(bytes::Bytes::from(raw))]);
    let mut events = EventStream::<serde_json::Value>::new(stream.boxed());

    let event = events.next().await.unwrap().unwrap();
    assert_eq!(event["type"], "error");
    assert_eq!(event["error"]["type"], "overloaded_error");
}

#[tokio::test]
async fn passes_through_managed_agents_stream_events() {
    // 对齐上游 0.109.0：Managed Agents 事件流的 event_start / event_delta 需放行
    let raw = "event: event_start\ndata: {\"type\":\"event_start\",\"event\":{\"id\":\"e1\"}}\n\n\
               event: event_delta\ndata: {\"type\":\"event_delta\",\"delta\":{\"text\":\"partial\"}}\n\n";

    let stream = stream::iter(vec![Ok(bytes::Bytes::from(raw))]);
    let mut events = EventStream::<serde_json::Value>::new(stream.boxed());

    let first = events.next().await.unwrap().unwrap();
    assert_eq!(first["type"], "event_start");

    let second = events.next().await.unwrap().unwrap();
    assert_eq!(second["type"], "event_delta");
}
