use serde::Deserialize;

use crate::auth::ModelsResponse;
use crate::conf::parse_protocol;
use crate::conf::Protocol;

/// A service published independently of the chat picker. No Free/Premium
/// inference, judge reuse, or fast-model fallback is allowed.
#[derive(Clone, Deserialize)]
pub struct SuggestionEndpoint {
    pub model: String,
    pub protocol: String,
    pub base_url: String,
    pub api_key: String,
    #[serde(default)]
    pub context_window: u32,
}

impl SuggestionEndpoint {
    pub fn from_catalog(response: &ModelsResponse) -> Option<Self> {
        // Services already round-trip in the catalog's flattened extras. Keep
        // the published cache shape intact instead of introducing a new cache.
        let value = response.extra.get("services")?.get("suggestion")?;
        let endpoint: Self = serde_json::from_value(value.clone()).ok()?;
        endpoint.parsed_protocol()?;
        if [&endpoint.model, &endpoint.base_url, &endpoint.api_key]
            .iter()
            .any(|value| value.trim().is_empty())
        {
            return None;
        }
        Some(endpoint)
    }

    pub fn parsed_protocol(&self) -> Option<Protocol> {
        parse_protocol(&self.protocol).ok()
    }
}

pub(super) async fn current() -> Option<SuggestionEndpoint> {
    crate::blocking::blocking_io("suggestion_config", read_current)
        .await
        .ok()
        .flatten()
}

fn read_current(
    cancel: &tokio_util::sync::CancellationToken,
) -> crate::error::Result<Option<SuggestionEndpoint>> {
    if crate::auth::load_auth()?.is_none() {
        return Ok(None);
    }
    if cancel.is_cancelled() {
        return Err(crate::blocking::cancelled("suggestion_config"));
    }
    Ok(crate::auth::load_models_cache()?
        .and_then(|cache| SuggestionEndpoint::from_catalog(&cache.response)))
}
