//! API 错误类型，对齐上游 `src/core/error.ts`。

use http::HeaderMap;
use serde_json::Value;
use thiserror::Error;

/// SDK 基础错误。
#[derive(Debug, Error)]
#[error("{0}")]
pub struct AnthropicError(pub String);

/// HTTP API 错误。
#[derive(Debug, Error)]
pub struct ApiError {
    pub status: Option<u16>,
    pub headers: HeaderMap,
    pub body: Value,
    pub request_id: Option<String>,
    pub error_type: Option<String>,
    message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl ApiError {
    pub fn new(
        status: Option<u16>,
        body: Value,
        message: Option<String>,
        headers: HeaderMap,
    ) -> Self {
        let error_type = body
            .get("error")
            .and_then(|e| e.get("type"))
            .and_then(|t| t.as_str())
            .map(str::to_owned);

        let request_id = headers
            .get("request-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);

        let msg = make_message(status, &body, message.as_deref());

        Self {
            status,
            headers,
            body,
            request_id,
            error_type,
            message: msg,
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// 根据 HTTP 状态码生成具体错误类型。
    pub fn generate(
        status: Option<u16>,
        body: Value,
        message: Option<String>,
        headers: HeaderMap,
    ) -> Error {
        if status.is_none() {
            return Error::Connection(ConnectionError {
                message: message.unwrap_or_else(|| "Connection error.".to_string()),
                source: None,
            });
        }

        let status = status.unwrap();
        let err = ApiError::new(Some(status), body, message, headers);

        match status {
            400 => Error::BadRequest(Box::new(err)),
            401 => Error::Authentication(Box::new(err)),
            403 => Error::PermissionDenied(Box::new(err)),
            404 => Error::NotFound(Box::new(err)),
            409 => Error::Conflict(Box::new(err)),
            422 => Error::UnprocessableEntity(Box::new(err)),
            429 => Error::RateLimit(Box::new(err)),
            500..=599 => Error::InternalServer(Box::new(err)),
            _ => Error::Api(Box::new(err)),
        }
    }
}

fn make_message(status: Option<u16>, body: &Value, message: Option<&str>) -> String {
    let msg = body
        .get("error")
        .and_then(|e| e.get("message"))
        .map(|m| {
            if let Some(s) = m.as_str() {
                s.to_string()
            } else {
                m.to_string()
            }
        })
        .or_else(|| {
            if body.is_null() || body.as_object().is_some_and(|o| o.is_empty()) {
                message.map(str::to_owned)
            } else {
                Some(body.to_string())
            }
        });

    match (status, msg) {
        (Some(s), Some(m)) => format!("{s} {m}"),
        (Some(s), None) => format!("{s} status code (no body)"),
        (None, Some(m)) => m,
        (None, None) => "(no status code or body)".to_string(),
    }
}

/// 用户主动中止请求。
#[derive(Debug, Error)]
#[error("{message}")]
pub struct UserAbortError {
    pub message: String,
}

/// 连接错误。
#[derive(Debug, Error)]
#[error("{message}")]
pub struct ConnectionError {
    pub message: String,
    #[source]
    pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

/// 连接超时。
#[derive(Debug, Error)]
#[error("{0}")]
pub struct ConnectionTimeoutError(pub String);

/// 可重试错误（中间件抛出以触发重试）。
#[derive(Debug, Error)]
#[error("{0}")]
pub struct RetryableError(pub String);

/// SDK 统一错误枚举。
#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Anthropic(#[from] AnthropicError),
    #[error("{0}")]
    Api(Box<ApiError>),
    #[error("{0}")]
    BadRequest(Box<ApiError>),
    #[error("{0}")]
    Authentication(Box<ApiError>),
    #[error("{0}")]
    PermissionDenied(Box<ApiError>),
    #[error("{0}")]
    NotFound(Box<ApiError>),
    #[error("{0}")]
    Conflict(Box<ApiError>),
    #[error("{0}")]
    UnprocessableEntity(Box<ApiError>),
    #[error("{0}")]
    RateLimit(Box<ApiError>),
    #[error("{0}")]
    InternalServer(Box<ApiError>),
    #[error(transparent)]
    UserAbort(#[from] UserAbortError),
    #[error(transparent)]
    Connection(#[from] ConnectionError),
    #[error(transparent)]
    ConnectionTimeout(#[from] ConnectionTimeoutError),
    #[error(transparent)]
    Retryable(#[from] RetryableError),
}

impl Error {
    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Api(e)
            | Error::BadRequest(e)
            | Error::Authentication(e)
            | Error::PermissionDenied(e)
            | Error::NotFound(e)
            | Error::Conflict(e)
            | Error::UnprocessableEntity(e)
            | Error::RateLimit(e)
            | Error::InternalServer(e) => e.status,
            // Box derefs automatically
            _ => None,
        }
    }

    pub fn request_id(&self) -> Option<&str> {
        match self {
            Error::Api(e)
            | Error::BadRequest(e)
            | Error::Authentication(e)
            | Error::PermissionDenied(e)
            | Error::NotFound(e)
            | Error::Conflict(e)
            | Error::UnprocessableEntity(e)
            | Error::RateLimit(e)
            | Error::InternalServer(e) => e.request_id.as_deref(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_429_to_rate_limit() {
        let err = ApiError::generate(
            Some(429),
            serde_json::json!({"error": {"type": "rate_limit_error", "message": "slow down"}}),
            None,
            HeaderMap::new(),
        );
        assert!(matches!(err, Error::RateLimit(_)));
    }

    #[test]
    fn maps_401_to_authentication() {
        let err = ApiError::generate(Some(401), serde_json::json!({}), None, HeaderMap::new());
        assert!(matches!(err, Error::Authentication(_)));
    }
}
