//! Request middleware, aligned with upstream `src/core/middleware.ts`.

use async_trait::async_trait;
use http::{HeaderMap, Method};
use serde_json::Value;

use crate::core::error::Error;

/// Middleware context.
#[derive(Debug, Clone)]
pub struct MiddlewareContext {
    pub method: Method,
    pub url: String,
    pub headers: HeaderMap,
    pub body: Option<Value>,
    pub retry_count: u32,
}

/// Middleware response.
#[derive(Debug, Clone)]
pub struct MiddlewareResponse {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Value,
}

/// Middleware trait.
#[async_trait]
pub trait Middleware: Send + Sync {
    async fn handle(
        &self,
        ctx: MiddlewareContext,
        next: &dyn NextMiddleware,
    ) -> Result<MiddlewareResponse, Error>;
}

/// Next handler in the middleware chain.
#[async_trait]
pub trait NextMiddleware: Send + Sync {
    async fn call(&self, ctx: MiddlewareContext) -> Result<MiddlewareResponse, Error>;
}

/// Terminal handler wrapper.
pub struct TerminalHandler<F>(pub F);

#[async_trait]
impl<F> NextMiddleware for TerminalHandler<F>
where
    F: Fn(MiddlewareContext) -> Result<MiddlewareResponse, Error> + Send + Sync,
{
    async fn call(&self, ctx: MiddlewareContext) -> Result<MiddlewareResponse, Error> {
        (self.0)(ctx)
    }
}

/// Run middleware chain sequentially.
pub async fn run_middleware_chain(
    middlewares: &[std::sync::Arc<dyn Middleware>],
    ctx: MiddlewareContext,
    terminal: impl NextMiddleware,
) -> Result<MiddlewareResponse, Error> {
    run_at(middlewares, 0, ctx, &terminal).await
}

async fn run_at(
    middlewares: &[std::sync::Arc<dyn Middleware>],
    index: usize,
    ctx: MiddlewareContext,
    terminal: &dyn NextMiddleware,
) -> Result<MiddlewareResponse, Error> {
    if index >= middlewares.len() {
        return terminal.call(ctx).await;
    }

    let next = ChainNext {
        middlewares,
        index: index + 1,
        terminal,
    };
    middlewares[index].handle(ctx, &next).await
}

struct ChainNext<'a> {
    middlewares: &'a [std::sync::Arc<dyn Middleware>],
    index: usize,
    terminal: &'a dyn NextMiddleware,
}

#[async_trait]
impl NextMiddleware for ChainNext<'_> {
    async fn call(&self, ctx: MiddlewareContext) -> Result<MiddlewareResponse, Error> {
        run_at(self.middlewares, self.index, ctx, self.terminal).await
    }
}
