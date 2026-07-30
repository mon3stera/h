#[derive(Debug, Clone, PartialEq)]
pub enum SseEvent<T> {
    Event(T),
    Done,
}

#[cfg(not(target_family = "wasm"))]
pub type StreamResponse<T> =
    std::pin::Pin<Box<dyn futures::Stream<Item = Result<T, crate::error::OpenAIError>> + Send>>;

#[cfg(target_family = "wasm")]
pub type StreamResponse<T> =
    std::pin::Pin<Box<dyn futures::Stream<Item = Result<T, crate::error::OpenAIError>>>>;

#[cfg(not(target_family = "wasm"))]
pub type SseResponse<T> = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<SseEvent<T>, crate::error::OpenAIError>> + Send>,
>;

#[cfg(target_family = "wasm")]
pub type SseResponse<T> =
    std::pin::Pin<Box<dyn futures::Stream<Item = Result<SseEvent<T>, crate::error::OpenAIError>>>>;
