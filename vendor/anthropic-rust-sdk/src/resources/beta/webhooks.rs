//! Beta Webhooks API.

use crate::client::Anthropic;
use crate::helpers::webhooks::UnwrapWebhookResult;

pub struct BetaWebhooks<'a> {
    _client: &'a Anthropic,
    _beta_headers: Vec<String>,
}

impl<'a> BetaWebhooks<'a> {
    pub(crate) fn new(client: &'a Anthropic, beta_headers: Vec<String>) -> Self {
        Self {
            _client: client,
            _beta_headers: beta_headers,
        }
    }

    pub async fn unwrap(
        &self,
        payload: &str,
        headers: &[(String, String)],
        secret: &str,
    ) -> Result<UnwrapWebhookResult, crate::core::error::Error> {
        crate::helpers::webhooks::unwrap_webhook(payload, headers, secret)
    }
}
