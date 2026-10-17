//! Builder for `StreamConfig` and stream event collection utilities.

use evotengine::provider::traits::*;
use evotengine::provider::ApiProtocol;
use evotengine::provider::ModelConfig;
use evotengine::provider::ModelOverrides;
use evotengine::provider::OpenAiCompat;
use evotengine::provider::ResolveModelRequest;
use evotengine::provider::RouteCapabilities;
use evotengine::types::*;

/// Builder for `StreamConfig` with sensible defaults.
pub struct StreamConfigBuilder {
    model: String,
    system_prompt: String,
    messages: Vec<Message>,
    tools: Vec<ToolDefinition>,
    thinking_level: ThinkingLevel,
    api_key: String,
    max_tokens: Option<u32>,
    model_config: Option<ModelConfig>,
    cache_config: CacheConfig,
    prompt_cache_key: Option<String>,
}

impl StreamConfigBuilder {
    /// Create a builder with minimal defaults.
    pub fn new() -> Self {
        Self {
            model: "test-model".into(),
            system_prompt: String::new(),
            messages: vec![Message::user("Hello")],
            tools: vec![],
            thinking_level: ThinkingLevel::Off,
            api_key: "test-key".into(),
            max_tokens: Some(1024),
            model_config: None,
            cache_config: CacheConfig::default(),
            prompt_cache_key: None,
        }
    }

    /// Anthropic-flavored defaults.
    pub fn anthropic() -> Self {
        Self::new()
            .model("claude-sonnet-4-20250514")
            .api_key("test-key")
    }

    /// OpenAI-flavored defaults.
    pub fn openai() -> Self {
        Self::new()
            .model("gpt-4o")
            .model_config(ModelConfig::openai("gpt-4o", "GPT-4o"))
    }

    pub fn model(mut self, model: &str) -> Self {
        self.model = model.into();
        self
    }

    pub fn system_prompt(mut self, prompt: &str) -> Self {
        self.system_prompt = prompt.into();
        self
    }

    pub fn messages(mut self, messages: Vec<Message>) -> Self {
        self.messages = messages;
        self
    }

    pub fn tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = tools;
        self
    }

    pub fn thinking(mut self, level: ThinkingLevel) -> Self {
        self.thinking_level = level;
        self
    }

    pub fn api_key(mut self, key: &str) -> Self {
        self.api_key = key.into();
        self
    }

    pub fn max_tokens(mut self, max: u32) -> Self {
        self.max_tokens = Some(max);
        self
    }

    pub fn no_max_tokens(mut self) -> Self {
        self.max_tokens = None;
        self
    }

    pub fn model_config(mut self, config: ModelConfig) -> Self {
        self.model_config = Some(config);
        self
    }

    pub fn cache_config(mut self, config: CacheConfig) -> Self {
        self.cache_config = config;
        self
    }

    pub fn cache_disabled(mut self) -> Self {
        self.cache_config = CacheConfig {
            enabled: false,
            strategy: CacheStrategy::Disabled,
        };
        self
    }

    pub fn prompt_cache_key(mut self, key: &str) -> Self {
        self.prompt_cache_key = Some(key.into());
        self
    }

    pub fn build(self) -> StreamConfig {
        StreamConfig {
            model: self.model,
            system_prompt: self.system_prompt,
            messages: self.messages,
            tools: self.tools,
            thinking_level: self.thinking_level,
            api_key: self.api_key,
            max_tokens: self.max_tokens,
            model_config: self.model_config,
            cache_config: self.cache_config,
            prompt_cache_key: self.prompt_cache_key,
            client: Some(dedicated_test_client()),
        }
    }
}

/// Every test config gets its own connection pool.
///
/// The shared process client pools keep-alive connections by origin, and these
/// tests run against hundreds of short-lived mock servers whose ephemeral ports
/// the OS recycles. A pooled connection belonging to a server that is gone can
/// then be handed to a different server's request, which shows up as a truncated
/// or missing response. Isolation removes that cross-test channel entirely.
fn dedicated_test_client() -> reqwest::Client {
    evotengine::provider::error::dedicated_client().unwrap_or_else(|_| reqwest::Client::new())
}

pub fn resolved_model_config(
    protocol: ApiProtocol,
    provider: &str,
    model_id: &str,
    base_url: &str,
    compat: Option<OpenAiCompat>,
    route_capabilities: RouteCapabilities,
    overrides: ModelOverrides,
) -> ModelConfig {
    ModelConfig::resolve(ResolveModelRequest {
        protocol,
        provider: provider.into(),
        model_id: model_id.into(),
        base_url: base_url.into(),
        headers: Default::default(),
        compat,
        route_capabilities,
        overrides,
    })
}

/// Collect all `StreamEvent`s from an unbounded receiver.
pub fn collect_stream_events(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<StreamEvent>,
) -> Vec<StreamEvent> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

/// Shorthand: create a tool definition.
pub fn tool_def(name: &str, desc: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description: desc.into(),
        parameters: serde_json::json!({"type": "object"}),
    }
}
