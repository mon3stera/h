//! Beta Messages API.

use crate::client::Anthropic;
use crate::core::error::Error;
use crate::resources::messages::{
    Message, MessageCountTokensParams, MessageCreateParams, MessageTokensCount,
};
use serde_json::Value;

pub struct BetaMessages<'a> {
    client: &'a Anthropic,
    beta_headers: Vec<String>,
}

impl<'a> BetaMessages<'a> {
    pub(crate) fn new(client: &'a Anthropic, beta_headers: Vec<String>) -> Self {
        Self {
            client,
            beta_headers,
        }
    }

    pub async fn create(&self, params: MessageCreateParams) -> Result<Message, Error> {
        let headers = crate::resources::messages::user_profile_headers(&params.user_profile_id);
        let mut body = serde_json::to_value(&params)
            .map_err(|e| crate::core::error::AnthropicError(format!("serialize error: {e}")))?;
        if let Value::Object(ref mut map) = body {
            map.insert("stream".to_string(), Value::Bool(false));
        }
        self.client
            .post_beta_with_headers("/v1/messages", &body, &self.beta_headers, headers.as_ref())
            .await
    }

    pub async fn count_tokens(
        &self,
        params: MessageCountTokensParams,
    ) -> Result<MessageTokensCount, Error> {
        let headers = crate::resources::messages::user_profile_headers(&params.user_profile_id);
        self.client
            .post_beta_with_headers(
                "/v1/messages/count_tokens",
                &params,
                &self.beta_headers,
                headers.as_ref(),
            )
            .await
    }
}
