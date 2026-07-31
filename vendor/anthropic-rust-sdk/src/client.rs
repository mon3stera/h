//! Anthropic API 客户端，对齐上游 `src/client.ts`。

use crate::core::error::{ApiError, ConnectionError, ConnectionTimeoutError, Error};
use crate::internal::backoff::{default_retry_timeout_ms, retry_after_ms};
use crate::internal::headers::{default_headers, merge_headers};
use crate::resources::beta::Beta;
use crate::resources::completions::Completions;
use crate::resources::messages::Messages;
use crate::resources::models::Models;
use http::HeaderMap;
use reqwest::{Client as HttpClient, Method, Response};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::HashMap;
use std::time::Duration;

/// 遗留 Text Completions 提示常量。
pub const HUMAN_PROMPT: &str = "\n\nHuman:";
pub const AI_PROMPT: &str = "\n\nAssistant:";

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);
const DEFAULT_MAX_RETRIES: u32 = 2;

/// 客户端配置。
#[derive(Debug, Clone)]
pub struct ClientOptions {
    pub api_key: Option<String>,
    pub auth_token: Option<String>,
    pub base_url: Option<String>,
    pub timeout: Option<Duration>,
    pub max_retries: Option<u32>,
    pub default_headers: HashMap<String, String>,
    pub default_query: HashMap<String, String>,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            api_key: std::env::var("ANTHROPIC_API_KEY").ok(),
            auth_token: std::env::var("ANTHROPIC_AUTH_TOKEN").ok(),
            base_url: std::env::var("ANTHROPIC_BASE_URL").ok(),
            timeout: None,
            max_retries: None,
            default_headers: HashMap::new(),
            default_query: HashMap::new(),
        }
    }
}

/// Anthropic API 客户端。
#[derive(Clone)]
pub struct Anthropic {
    http: HttpClient,
    api_key: Option<String>,
    auth_token: Option<String>,
    base_url: String,
    #[allow(dead_code)]
    timeout: Duration,
    max_retries: u32,
    default_headers: HashMap<String, String>,
    #[allow(dead_code)]
    default_query: HashMap<String, String>,
    #[allow(dead_code)]
    middleware: Vec<std::sync::Arc<dyn crate::core::middleware::Middleware>>,
}

impl Anthropic {
    pub fn new() -> Result<Self, Error> {
        Self::with_options(ClientOptions::default())
    }

    pub fn with_options(options: ClientOptions) -> Result<Self, Error> {
        let (api_key, auth_token) = (
            options
                .api_key
                .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok()),
            options
                .auth_token
                .or_else(|| std::env::var("ANTHROPIC_AUTH_TOKEN").ok()),
        );
        if api_key.is_none() && auth_token.is_none() {
            return Err(Error::Anthropic(crate::core::error::AnthropicError(
                "Missing authentication: set ANTHROPIC_API_KEY or ANTHROPIC_AUTH_TOKEN, or pass api_key/auth_token in ClientOptions"
                    .into(),
            )));
        }

        let base_url = options
            .base_url
            .or_else(|| std::env::var("ANTHROPIC_BASE_URL").ok())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

        let timeout = options.timeout.unwrap_or(DEFAULT_TIMEOUT);
        let max_retries = options.max_retries.unwrap_or(DEFAULT_MAX_RETRIES);

        let http = HttpClient::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| {
                Error::Connection(ConnectionError {
                    message: e.to_string(),
                    source: Some(Box::new(e)),
                })
            })?;

        Ok(Self {
            http,
            api_key,
            auth_token,
            base_url: base_url.trim_end_matches('/').to_string(),
            timeout,
            max_retries,
            default_headers: options.default_headers,
            default_query: options.default_query,
            middleware: Vec::new(),
        })
    }

    pub fn with_api_key(api_key: impl Into<String>) -> Result<Self, Error> {
        Self::with_options(ClientOptions {
            api_key: Some(api_key.into()),
            ..Default::default()
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn max_retries(&self) -> u32 {
        self.max_retries
    }

    pub fn messages(&self) -> Messages<'_> {
        Messages::new(self)
    }

    pub fn models(&self) -> Models<'_> {
        Models::new(self)
    }

    pub fn completions(&self) -> Completions<'_> {
        Completions::new(self)
    }

    pub fn beta(&self) -> Beta<'_> {
        Beta::new(self)
    }

    pub(crate) async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        self.request(Method::GET, path, None::<&()>, false, None)
            .await
    }

    pub(crate) async fn get_with_query<T: DeserializeOwned>(
        &self,
        path: &str,
        query: Option<&[(&str, &str)]>,
    ) -> Result<T, Error> {
        let mut url = self.build_url(path);
        if let Some(q) = query {
            let qs: Vec<String> = q
                .iter()
                .map(|(k, v)| format!("{}={}", k, urlencoding_encode(v)))
                .collect();
            if !qs.is_empty() {
                url.push('?');
                url.push_str(&qs.join("&"));
            }
        }
        self.request_url(Method::GET, &url, None::<&()>, false, None)
            .await
    }

    pub(crate) async fn post<T, B>(&self, path: &str, body: &B) -> Result<T, Error>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        self.request(Method::POST, path, Some(body), false, None)
            .await
    }

    /// POST 请求并附加额外请求头（如 `anthropic-user-profile-id`）。
    pub(crate) async fn post_with_headers<T, B>(
        &self,
        path: &str,
        body: &B,
        extra_headers: Option<&HashMap<String, String>>,
    ) -> Result<T, Error>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        self.request(Method::POST, path, Some(body), false, extra_headers)
            .await
    }

    /// 流式 POST 请求并附加额外请求头。
    pub(crate) async fn post_streaming_with_headers<B>(
        &self,
        path: &str,
        body: &B,
        extra_headers: Option<&HashMap<String, String>>,
    ) -> Result<Response, Error>
    where
        B: Serialize + ?Sized,
    {
        self.request_raw(Method::POST, path, Some(body), true, extra_headers)
            .await
    }

    pub(crate) async fn post_empty<T>(&self, path: &str) -> Result<T, Error>
    where
        T: DeserializeOwned,
    {
        self.request(Method::POST, path, None::<&()>, false, None)
            .await
    }

    #[allow(dead_code)]
    pub(crate) async fn delete<T>(&self, path: &str) -> Result<T, Error>
    where
        T: DeserializeOwned,
    {
        self.request(Method::DELETE, path, None::<&()>, false, None)
            .await
    }

    #[allow(dead_code)]
    pub(crate) async fn patch<T, B>(&self, path: &str, body: &B) -> Result<T, Error>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        self.request(Method::PATCH, path, Some(body), false, None)
            .await
    }

    pub(crate) async fn get_beta<T>(
        &self,
        path: &str,
        beta_headers: &[String],
        query: Option<&[(&str, &str)]>,
    ) -> Result<T, Error>
    where
        T: DeserializeOwned,
    {
        self.request_beta(
            Method::GET,
            path,
            None::<&()>,
            beta_headers,
            query,
            false,
            None,
        )
        .await
    }

    pub(crate) async fn post_beta<T, B>(
        &self,
        path: &str,
        body: &B,
        beta_headers: &[String],
    ) -> Result<T, Error>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        self.request_beta(
            Method::POST,
            path,
            Some(body),
            beta_headers,
            None,
            false,
            None,
        )
        .await
    }

    /// Beta POST 请求并附加额外请求头（如 `anthropic-user-profile-id`）。
    pub(crate) async fn post_beta_with_headers<T, B>(
        &self,
        path: &str,
        body: &B,
        beta_headers: &[String],
        additional_headers: Option<&HashMap<String, String>>,
    ) -> Result<T, Error>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        self.request_beta(
            Method::POST,
            path,
            Some(body),
            beta_headers,
            None,
            false,
            additional_headers,
        )
        .await
    }

    pub(crate) async fn delete_beta<T>(
        &self,
        path: &str,
        beta_headers: &[String],
    ) -> Result<T, Error>
    where
        T: DeserializeOwned,
    {
        self.request_beta(
            Method::DELETE,
            path,
            None::<&()>,
            beta_headers,
            None,
            false,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn request_beta<T, B>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        beta_headers: &[String],
        query: Option<&[(&str, &str)]>,
        stream: bool,
        additional_headers: Option<&HashMap<String, String>>,
    ) -> Result<T, Error>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let mut extra_headers = self.default_headers.clone();
        if !beta_headers.is_empty() {
            extra_headers.insert("anthropic-beta".to_string(), beta_headers.join(","));
        }
        if let Some(additional) = additional_headers {
            for (k, v) in additional {
                extra_headers.insert(k.clone(), v.clone());
            }
        }
        let url = self.build_url(path);
        let mut full_url = url;
        if let Some(q) = query {
            let qs: Vec<String> = q
                .iter()
                .map(|(k, v)| format!("{}={}", k, urlencoding_encode(v)))
                .collect();
            if !qs.is_empty() {
                full_url.push('?');
                full_url.push_str(&qs.join("&"));
            }
        }

        let response = self
            .make_request_with_retries_beta(
                method,
                &full_url,
                body,
                stream,
                self.max_retries,
                &extra_headers,
            )
            .await?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let bytes = response.bytes().await.map_err(|e| {
            Error::Connection(ConnectionError {
                message: e.to_string(),
                source: Some(Box::new(e)),
            })
        })?;

        if !(200..300).contains(&status) {
            let body_json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
            return Err(ApiError::generate(
                Some(status),
                body_json,
                None,
                header_map_from_reqwest(&headers),
            ));
        }

        serde_json::from_slice(&bytes).map_err(|e| {
            Error::Anthropic(crate::core::error::AnthropicError(format!(
                "failed to parse response JSON: {e}"
            )))
        })
    }

    async fn make_request_with_retries_beta<B>(
        &self,
        method: Method,
        url: &str,
        body: Option<&B>,
        stream: bool,
        mut retries_remaining: u32,
        extra_headers: &HashMap<String, String>,
    ) -> Result<Response, Error>
    where
        B: Serialize + ?Sized,
    {
        loop {
            let mut headers = self.build_headers(stream)?;
            merge_headers(&mut headers, extra_headers);
            let mut req = self.http.request(method.clone(), url).headers(headers);

            if let Some(b) = body {
                req = req.json(b);
            }

            let response = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    if e.is_timeout() {
                        return Err(Error::ConnectionTimeout(ConnectionTimeoutError(
                            e.to_string(),
                        )));
                    }
                    if retries_remaining == 0 {
                        return Err(Error::Connection(ConnectionError {
                            message: e.to_string(),
                            source: Some(Box::new(e)),
                        }));
                    }
                    retries_remaining -= 1;
                    tokio::time::sleep(Duration::from_millis(default_retry_timeout_ms(
                        retries_remaining,
                        self.max_retries,
                    )))
                    .await;
                    continue;
                }
            };

            let status = response.status().as_u16();
            if (200..300).contains(&status) {
                return Ok(response);
            }

            if retries_remaining == 0 || !should_retry(status, response.headers()) {
                return Ok(response);
            }

            let wait = retry_after_ms(response.headers()).unwrap_or_else(|| {
                default_retry_timeout_ms(retries_remaining - 1, self.max_retries)
            });
            retries_remaining -= 1;
            tokio::time::sleep(Duration::from_millis(wait)).await;
        }
    }

    async fn request<T, B>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        stream: bool,
        extra_headers: Option<&HashMap<String, String>>,
    ) -> Result<T, Error>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let response = self
            .request_raw(method.clone(), path, body, stream, extra_headers)
            .await?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let bytes = response.bytes().await.map_err(|e| {
            Error::Connection(ConnectionError {
                message: e.to_string(),
                source: Some(Box::new(e)),
            })
        })?;

        if !(200..300).contains(&status) {
            let body_json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
            return Err(ApiError::generate(
                Some(status),
                body_json,
                None,
                header_map_from_reqwest(&headers),
            ));
        }

        serde_json::from_slice(&bytes).map_err(|e| {
            Error::Anthropic(crate::core::error::AnthropicError(format!(
                "failed to parse response JSON: {e}"
            )))
        })
    }

    async fn request_url<T, B>(
        &self,
        method: Method,
        url: &str,
        body: Option<&B>,
        stream: bool,
        extra_headers: Option<&HashMap<String, String>>,
    ) -> Result<T, Error>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let response = self
            .make_request_with_retries(method, url, body, stream, self.max_retries, extra_headers)
            .await?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let bytes = response.bytes().await.map_err(|e| {
            Error::Connection(ConnectionError {
                message: e.to_string(),
                source: Some(Box::new(e)),
            })
        })?;

        if !(200..300).contains(&status) {
            let body_json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
            return Err(ApiError::generate(
                Some(status),
                body_json,
                None,
                header_map_from_reqwest(&headers),
            ));
        }

        serde_json::from_slice(&bytes).map_err(|e| {
            Error::Anthropic(crate::core::error::AnthropicError(format!(
                "failed to parse response JSON: {e}"
            )))
        })
    }

    async fn request_raw<B>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        stream: bool,
        extra_headers: Option<&HashMap<String, String>>,
    ) -> Result<Response, Error>
    where
        B: Serialize + ?Sized,
    {
        let url = self.build_url(path);
        self.make_request_with_retries(method, &url, body, stream, self.max_retries, extra_headers)
            .await
    }

    fn build_url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn build_headers(&self, stream: bool) -> Result<HeaderMap, Error> {
        let mut headers = default_headers(self.api_key.as_deref());
        if let Some(token) = &self.auth_token {
            headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
        }
        if stream {
            headers.insert("accept", "text/event-stream".parse().unwrap());
        } else {
            headers.insert("accept", "application/json".parse().unwrap());
        }
        headers.insert("content-type", "application/json".parse().unwrap());
        merge_headers(&mut headers, &self.default_headers);
        Ok(headers)
    }

    async fn make_request_with_retries<B>(
        &self,
        method: Method,
        url: &str,
        body: Option<&B>,
        stream: bool,
        mut retries_remaining: u32,
        extra_headers: Option<&HashMap<String, String>>,
    ) -> Result<Response, Error>
    where
        B: Serialize + ?Sized,
    {
        loop {
            let mut headers = self.build_headers(stream)?;
            if let Some(extra) = extra_headers {
                merge_headers(&mut headers, extra);
            }
            let mut req = self.http.request(method.clone(), url).headers(headers);

            if let Some(b) = body {
                req = req.json(b);
            }

            let response = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    if e.is_timeout() {
                        return Err(Error::ConnectionTimeout(ConnectionTimeoutError(
                            e.to_string(),
                        )));
                    }
                    if retries_remaining == 0 {
                        return Err(Error::Connection(ConnectionError {
                            message: e.to_string(),
                            source: Some(Box::new(e)),
                        }));
                    }
                    retries_remaining -= 1;
                    tokio::time::sleep(Duration::from_millis(default_retry_timeout_ms(
                        retries_remaining,
                        self.max_retries,
                    )))
                    .await;
                    continue;
                }
            };

            let status = response.status().as_u16();
            if (200..300).contains(&status) {
                return Ok(response);
            }

            if retries_remaining == 0 || !should_retry(status, response.headers()) {
                return Ok(response);
            }

            let wait = retry_after_ms(response.headers()).unwrap_or_else(|| {
                default_retry_timeout_ms(retries_remaining - 1, self.max_retries)
            });
            retries_remaining -= 1;
            tokio::time::sleep(Duration::from_millis(wait)).await;
        }
    }
}

impl Default for Anthropic {
    fn default() -> Self {
        Self::new().expect("ANTHROPIC_API_KEY must be set for default client")
    }
}

fn should_retry(status: u16, headers: &reqwest::header::HeaderMap) -> bool {
    if let Some(v) = headers.get("x-should-retry") {
        if let Ok(s) = v.to_str() {
            if s == "true" {
                return true;
            }
            if s == "false" {
                return false;
            }
        }
    }

    matches!(status, 408 | 409 | 429) || (500..600).contains(&status)
}

fn header_map_from_reqwest(headers: &reqwest::header::HeaderMap) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (k, v) in headers.iter() {
        if let Ok(val) = http::HeaderValue::from_bytes(v.as_bytes()) {
            map.insert(k.clone(), val);
        }
    }
    map
}

fn urlencoding_encode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            _ => format!("%{:02X}", c as u8),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_retry_rate_limit() {
        let headers = reqwest::header::HeaderMap::new();
        assert!(should_retry(429, &headers));
    }

    #[test]
    fn should_not_retry_bad_request() {
        let headers = reqwest::header::HeaderMap::new();
        assert!(!should_retry(400, &headers));
    }
}
