//! Beta Tunnels API（MCP Tunnels），对齐上游 `src/resources/beta/tunnels/`。

use crate::client::Anthropic;
use crate::core::error::Error;
use crate::core::pagination::PageCursor;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Tunnel 资源对象。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BetaTunnel {
    pub id: String,
    #[serde(flatten)]
    pub extra: Value,
}

/// Tunnel 连接令牌，`reveal_token` / `rotate_token` 的响应。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BetaTunnelToken {
    pub id: String,
    #[serde(flatten)]
    pub extra: Value,
}

/// Tunnel 证书资源对象。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BetaTunnelCertificate {
    pub id: String,
    #[serde(flatten)]
    pub extra: Value,
}

/// Beta Tunnels API 资源。
pub struct BetaTunnels<'a> {
    client: &'a Anthropic,
    beta_headers: Vec<String>,
}

impl<'a> BetaTunnels<'a> {
    pub(crate) fn new(client: &'a Anthropic, beta_headers: Vec<String>) -> Self {
        Self {
            client,
            beta_headers,
        }
    }

    /// 隧道证书子资源，对齐上游 `client.beta.tunnels.certificates`。
    pub fn certificates(&self) -> BetaTunnelCertificates<'a> {
        BetaTunnelCertificates::new(self.client, self.beta_headers.clone())
    }

    pub async fn create(&self, body: &Value) -> Result<BetaTunnel, Error> {
        self.client
            .post_beta("/v1/tunnels", body, &self.beta_headers)
            .await
    }

    pub async fn retrieve(&self, tunnel_id: &str) -> Result<BetaTunnel, Error> {
        self.client
            .get_beta(
                &format!("/v1/tunnels/{tunnel_id}"),
                &self.beta_headers,
                None,
            )
            .await
    }

    pub async fn list(&self) -> Result<PageCursor<BetaTunnel>, Error> {
        self.client
            .get_beta("/v1/tunnels", &self.beta_headers, None)
            .await
    }

    /// 归档一个隧道（无请求体的 POST）。
    pub async fn archive(&self, tunnel_id: &str) -> Result<BetaTunnel, Error> {
        self.client
            .post_beta(
                &format!("/v1/tunnels/{tunnel_id}/archive"),
                &serde_json::json!({}),
                &self.beta_headers,
            )
            .await
    }

    /// 显示隧道当前连接令牌（无请求体的 POST）。
    pub async fn reveal_token(&self, tunnel_id: &str) -> Result<BetaTunnelToken, Error> {
        self.client
            .post_beta(
                &format!("/v1/tunnels/{tunnel_id}/reveal_token"),
                &serde_json::json!({}),
                &self.beta_headers,
            )
            .await
    }

    /// 轮换隧道连接令牌。
    pub async fn rotate_token(
        &self,
        tunnel_id: &str,
        body: &Value,
    ) -> Result<BetaTunnelToken, Error> {
        self.client
            .post_beta(
                &format!("/v1/tunnels/{tunnel_id}/rotate_token"),
                body,
                &self.beta_headers,
            )
            .await
    }
}

/// Beta Tunnel Certificates API 子资源。
pub struct BetaTunnelCertificates<'a> {
    client: &'a Anthropic,
    beta_headers: Vec<String>,
}

impl<'a> BetaTunnelCertificates<'a> {
    pub(crate) fn new(client: &'a Anthropic, beta_headers: Vec<String>) -> Self {
        Self {
            client,
            beta_headers,
        }
    }

    pub async fn create(
        &self,
        tunnel_id: &str,
        body: &Value,
    ) -> Result<BetaTunnelCertificate, Error> {
        self.client
            .post_beta(
                &format!("/v1/tunnels/{tunnel_id}/certificates"),
                body,
                &self.beta_headers,
            )
            .await
    }

    pub async fn retrieve(
        &self,
        tunnel_id: &str,
        certificate_id: &str,
    ) -> Result<BetaTunnelCertificate, Error> {
        self.client
            .get_beta(
                &format!("/v1/tunnels/{tunnel_id}/certificates/{certificate_id}"),
                &self.beta_headers,
                None,
            )
            .await
    }

    pub async fn list(&self, tunnel_id: &str) -> Result<PageCursor<BetaTunnelCertificate>, Error> {
        self.client
            .get_beta(
                &format!("/v1/tunnels/{tunnel_id}/certificates"),
                &self.beta_headers,
                None,
            )
            .await
    }

    /// 归档一个隧道证书（无请求体的 POST）。
    pub async fn archive(
        &self,
        tunnel_id: &str,
        certificate_id: &str,
    ) -> Result<BetaTunnelCertificate, Error> {
        self.client
            .post_beta(
                &format!("/v1/tunnels/{tunnel_id}/certificates/{certificate_id}/archive"),
                &serde_json::json!({}),
                &self.beta_headers,
            )
            .await
    }
}
