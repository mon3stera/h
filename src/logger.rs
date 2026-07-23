use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

const DEFAULT_FILTER: &str = "warn,h=info,async_openai=off,hyper=off,hyper_util=off";

fn env_filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER))
}

pub fn init(directory: impl AsRef<Path>) -> anyhow::Result<WorkerGuard> {
    let appender = tracing_appender::rolling::daily(directory, "h.log");

    let (writer, guard) = tracing_appender::non_blocking(appender);

    let filter = env_filter();

    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .with_writer(writer)
                .with_ansi(false)
                .with_target(true)
                .with_thread_ids(false),
        )
        .try_init()?;

    Ok(guard)
}

#[cfg(test)]
mod tests {
    use super::DEFAULT_FILTER;

    #[test]
    fn default_filter_is_app_focused() {
        assert!(DEFAULT_FILTER.contains("h=info"));
        assert!(DEFAULT_FILTER.contains("async_openai=off"));
        assert!(DEFAULT_FILTER.contains("hyper=off"));
        assert!(!DEFAULT_FILTER.contains("h=debug"));
    }
}
