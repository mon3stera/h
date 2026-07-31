//! 退避与抖动工具，对齐上游 `src/internal/utils/backoff.ts`。

use crate::core::error::Error;

/// 判断是否为指定 HTTP 状态码的 API 错误。
pub fn is_status(err: &Error, code: u16) -> bool {
    err.status() == Some(code)
}

/// 判断是否为 4xx 客户端错误。
pub fn is_4xx(err: &Error) -> bool {
    matches!(
        err.status(),
        Some(s) if (400..500).contains(&s)
    )
}

/// 不可重试的 4xx（408、409、429 除外）。
pub fn is_fatal_4xx(err: &Error) -> bool {
    is_4xx(err) && !is_status(err, 408) && !is_status(err, 409) && !is_status(err, 429)
}

/// 指数退避：`base_ms * 2^attempt`，上限 `cap_ms`。
pub fn backoff(attempt: u32, base_ms: u64, cap_ms: u64) -> u64 {
    let exp = base_ms.saturating_mul(2u64.saturating_pow(attempt));
    exp.min(cap_ms)
}

/// 在 `[low_ms, high_ms)` 区间内均匀随机延迟。
pub fn jitter(low_ms: u64, high_ms: u64) -> u64 {
    let range = high_ms.saturating_sub(low_ms);
    if range == 0 {
        return low_ms;
    }
    low_ms + (rand_fraction() * range as f64) as u64
}

/// 对延迟施加最多 25% 的随机削减，避免惊群。
pub fn apply_jitter(ms: u64) -> u64 {
    (ms as f64 * (1.0 - rand_fraction() * 0.25)) as u64
}

/// 默认重试等待时间（毫秒），对齐 TS `calculateDefaultRetryTimeoutMillis`。
pub fn default_retry_timeout_ms(retries_remaining: u32, max_retries: u32) -> u64 {
    let initial_retry_delay = 0.5_f64;
    let max_retry_delay = 8.0_f64;
    let num_retries = max_retries.saturating_sub(retries_remaining);
    let sleep_seconds = (initial_retry_delay * 2f64.powi(num_retries as i32)).min(max_retry_delay);
    apply_jitter((sleep_seconds * 1000.0) as u64)
}

/// 从响应头解析重试等待时间。
pub fn retry_after_ms(headers: &http::HeaderMap) -> Option<u64> {
    if let Some(v) = headers.get("retry-after-ms") {
        if let Ok(s) = v.to_str() {
            if let Ok(ms) = s.parse::<f64>() {
                return Some(ms as u64);
            }
        }
    }

    if let Some(v) = headers.get("retry-after") {
        if let Ok(s) = v.to_str() {
            if let Ok(secs) = s.parse::<f64>() {
                return Some((secs * 1000.0) as u64);
            }
            if let Some(ts) = chrono_parse_retry_after(s) {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64;
                return Some((ts - now).max(0) as u64);
            }
        }
    }

    None
}

fn chrono_parse_retry_after(s: &str) -> Option<i64> {
    // 简化 HTTP-date 解析：仅处理数字秒，日期格式回退为 None
    let _ = s;
    None
}

fn rand_fraction() -> f64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::time::SystemTime::now().hash(&mut hasher);
    (hasher.finish() % 10_000) as f64 / 10_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exponential_backoff_caps() {
        assert_eq!(backoff(0, 500, 8000), 500);
        assert_eq!(backoff(10, 500, 8000), 8000);
    }
}
