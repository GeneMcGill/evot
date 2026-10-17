//! Network loss and half-open sockets must recover without user intervention.

use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use evotengine::provider::stream::sse::SseReader;
use evotengine::provider::MockProvider;
use evotengine::provider::ProviderError;
use evotengine::provider::StreamConfig;
use evotengine::provider::StreamEvent;
use evotengine::provider::StreamOutcome;
use evotengine::provider::StreamProvider;
use evotengine::types::AgentContext;
use evotengine::*;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::fixtures::agent_harness::collect_events;
use crate::fixtures::agent_harness::make_config;

struct NetworkRecoveryProvider {
    restored_at: tokio::time::Instant,
    half_open: bool,
    calls: AtomicUsize,
    inner: MockProvider,
}

#[async_trait::async_trait]
impl StreamProvider for NetworkRecoveryProvider {
    async fn stream(
        &self,
        config: StreamConfig,
        tx: mpsc::UnboundedSender<StreamEvent>,
        cancel: CancellationToken,
    ) -> Result<StreamOutcome, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if tokio::time::Instant::now() < self.restored_at {
            if self.half_open {
                // The existing connection stays dead even after the network
                // returns. Only a new request can recover.
                let mut reader = SseReader::from_stream(futures::stream::pending());
                reader.next(&cancel).await.map_err(ProviderError::Network)?;
            }
            return Err(ProviderError::Network("network unavailable".into()));
        }
        self.inner.stream(config, tx, cancel).await
    }
}

#[tokio::test(start_paused = true)]
async fn network_returns_after_half_an_hour_and_recovers_without_esc() {
    for half_open in [false, true] {
        let restored_at = tokio::time::Instant::now() + Duration::from_secs(30 * 60);
        let provider = Arc::new(NetworkRecoveryProvider {
            restored_at,
            half_open,
            calls: AtomicUsize::new(0),
            inner: MockProvider::text("automatically recovered"),
        });
        let mut config = make_config(MockProvider::text("unused"));
        config.provider = provider.clone();
        config.retry_policy = RetryPolicy::new(5);
        let mut context = AgentContext {
            system_prompt: "test".into(),
            messages: Vec::new(),
            tools: Vec::new(),
            cwd: std::path::PathBuf::new(),
            path_guard: Arc::new(PathGuard::open()),
            prompt_cache_key: None,
        };
        let (tx, rx) = mpsc::unbounded_channel();
        let messages = agent_loop(
            vec![Message::user("continue my work").into()],
            &mut context,
            &config,
            tx,
            CancellationToken::new(),
        )
        .await;

        // A failed probe can overlap recovery: allow its remaining 60-second
        // idle timeout plus the next 10-second probe interval, never minutes.
        let recovery_delay = tokio::time::Instant::now().duration_since(restored_at);
        let bound = if half_open { 70 } else { 10 };
        assert!(
            recovery_delay <= Duration::from_secs(bound),
            "{recovery_delay:?}"
        );
        assert!(provider.calls.load(Ordering::SeqCst) > 6);
        assert_eq!(
            messages.len(),
            2,
            "retries must not add failed messages to history"
        );
        assert!(matches!(
            messages.last(),
            Some(AgentMessage::Llm(Message::Assistant { content, stop_reason: StopReason::Stop, .. }))
            if matches!(content.first(), Some(Content::Text { text }) if text == "automatically recovered")
        ));
        let events = collect_events(rx);
        let retry_delays: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::LlmCallRetry { delay_ms, .. } => Some(*delay_ms),
                _ => None,
            })
            .collect();
        assert_eq!(retry_delays.len(), 5);
        assert!(retry_delays.iter().all(|delay| *delay <= 10_000));
        let outage_delays: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::OutageWait { delay_ms, .. } => Some(*delay_ms),
                _ => None,
            })
            .collect();
        assert!(!outage_delays.is_empty());
        assert!(outage_delays.iter().all(|delay| *delay == 10_000));
        assert!(!events
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolExecutionStart { .. })));
    }
}

#[tokio::test(start_paused = true)]
async fn server_outages_keep_their_existing_quiet_probe_cadence() {
    let provider = Arc::new(super::common::FailThenSucceedProvider {
        fail_count: AtomicUsize::new(0),
        max_failures: 3,
        error: ProviderError::Transient {
            message: "service unavailable".into(),
        },
        inner: MockProvider::text("recovered"),
    });
    let mut config = make_config(MockProvider::text("unused"));
    config.provider = provider;
    config.retry_policy = RetryPolicy::new(1);
    let mut context = AgentContext {
        system_prompt: "test".into(),
        messages: Vec::new(),
        tools: Vec::new(),
        cwd: std::path::PathBuf::new(),
        path_guard: Arc::new(PathGuard::open()),
        prompt_cache_key: None,
    };
    let (tx, rx) = mpsc::unbounded_channel();
    agent_loop(
        vec![Message::user("hi").into()],
        &mut context,
        &config,
        tx,
        CancellationToken::new(),
    )
    .await;
    let delays: Vec<_> = collect_events(rx)
        .iter()
        .filter_map(|event| match event {
            AgentEvent::OutageWait { delay_ms, .. } => Some(*delay_ms),
            _ => None,
        })
        .collect();
    assert_eq!(delays, vec![60_000, 60_000]);
}
