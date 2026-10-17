//! Provider tests run against many short-lived mock servers on recycled ports.
//! They must never share a connection pool: a keep-alive connection belonging to
//! a server that is gone can otherwise be handed to another server's request,
//! which shows up as a truncated or missing streamed response.

use evotengine::provider::ProviderError;
use evotengine::provider::StreamConfig;
use evotengine::provider::StreamProvider;

#[test]
fn built_configs_carry_their_own_connection_pool() {
    let first = super::fixtures::stream_config::StreamConfigBuilder::openai().build();
    let second = super::fixtures::stream_config::StreamConfigBuilder::openai().build();
    assert!(first.client.is_some(), "fixture configs must be isolated");
    assert!(second.client.is_some(), "fixture configs must be isolated");
}

#[test]
fn configs_without_an_explicit_client_fall_back_to_the_shared_one() {
    let config = StreamConfig {
        model: "m".into(),
        system_prompt: String::new(),
        messages: Vec::new(),
        tools: Vec::new(),
        thinking_level: Default::default(),
        api_key: "k".into(),
        max_tokens: None,
        model_config: None,
        cache_config: Default::default(),
        prompt_cache_key: None,
        client: None,
    };
    assert!(config.http_client().is_ok());
}

/// Providers must route through the config's client, not the process client.
#[tokio::test]
async fn a_provider_request_uses_the_configured_client() -> Result<(), Box<dyn std::error::Error>> {
    use tokio_util::sync::CancellationToken;
    use wiremock::matchers::method;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server)
        .await;
    let mut config = super::fixtures::stream_config::StreamConfigBuilder::openai()
        .model_config(super::fixtures::stream_config::resolved_model_config(
            evotengine::provider::ApiProtocol::OpenAiCompletions,
            "openai",
            "test-model",
            &server.uri(),
            Default::default(),
            Default::default(),
            Default::default(),
        ))
        .build();
    // A client that cannot reach anything: the request must fail, proving the
    // provider used this client rather than the shared one.
    config.client = Some(
        reqwest::Client::builder()
            .proxy(reqwest::Proxy::all("http://127.0.0.1:1")?)
            .build()?,
    );
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let result = evotengine::provider::OpenAiCompatProvider
        .stream(config, tx, CancellationToken::new())
        .await;
    let error = result
        .err()
        .ok_or("the request should not have succeeded")?;
    assert!(matches!(error, ProviderError::Network(_)), "{error:?}");
    assert_eq!(
        server.received_requests().await.unwrap_or_default().len(),
        0
    );
    Ok(())
}
