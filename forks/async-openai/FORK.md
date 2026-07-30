# Local fork

This directory is based on `async-openai` 0.41.1.

The local patch adds an opt-in Responses SSE stream that preserves the
`data: [DONE]` transport sentinel as `SseEvent::Done`. Existing stream methods
keep their upstream behavior and still end without yielding the sentinel.
