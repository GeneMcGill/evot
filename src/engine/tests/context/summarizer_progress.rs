use std::sync::Arc;

use async_trait::async_trait;
use evotengine::context::compaction::summarizer::llm;
use evotengine::context::compaction::summarizer::mode::SummarizerContext;
use evotengine::context::compaction::summarizer::types::SummarizerInput;
use evotengine::context::compaction::types::CompactionActivity;
use evotengine::context::compaction::types::CompactionObserver;
use evotengine::context::compaction::types::FileOps;
use evotengine::provider::ProviderError;
use evotengine::provider::StreamConfig;
use evotengine::provider::StreamEvent;
use evotengine::provider::StreamOutcome;
use evotengine::provider::StreamProvider;
use evotengine::types::CacheConfig;
use evotengine::types::Content;
use evotengine::types::Message;
use evotengine::types::StopReason;
use evotengine::types::ThinkingLevel;
use evotengine::types::Usage;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

struct ProgressProvider {
    events: Vec<StreamEvent>,
    requests: Mutex<Vec<StreamConfig>>,
    generated: Option<Arc<Notify>>,
    retained_sender: Mutex<Option<mpsc::UnboundedSender<StreamEvent>>>,
}

#[async_trait]
impl StreamProvider for ProgressProvider {
    async fn stream(
        &self,
        config: StreamConfig,
        tx: mpsc::UnboundedSender<StreamEvent>,
        _cancel: CancellationToken,
    ) -> Result<StreamOutcome, ProviderError> {
        self.requests.lock().push(config);
        for event in &self.events {
            let _ = tx.send(event.clone());
        }
        if let Some(generated) = &self.generated {
            // Cannot return until the observer has consumed a live delta.
            generated.notified().await;
            *self.retained_sender.lock() = Some(tx);
        } else {
            // Exercise a closed event channel while the provider is pending.
            drop(tx);
            tokio::task::yield_now().await;
        }
        Ok(StreamOutcome::complete(Message::Assistant {
            content: vec![Content::Text {
                text: "authoritative summary".into(),
            }],
            stop_reason: StopReason::Stop,
            model: "test".into(),
            provider: "test".into(),
            usage: Usage::default(),
            timestamp: 0,
            error_message: None,
            response_id: None,
        }))
    }
}

fn input() -> SummarizerInput {
    SummarizerInput {
        conversation: "[User]: preserve the constraints".into(),
        turn_prefix: None,
        previous_summary: None,
        custom_instructions: None,
        request_max_bytes: 512 * 1024,
        file_ops: FileOps::default(),
        evicted_count: 1,
        completed_requests: vec![],
        env_discoveries: vec![],
        last_conclusion: None,
    }
}

fn context(provider: Arc<ProgressProvider>, thinking_level: ThinkingLevel) -> SummarizerContext {
    SummarizerContext {
        provider,
        model: "test".into(),
        api_key: "test-key".into(),
        thinking_level,
        system_prompt: "normal turn instructions".into(),
        tools: vec![],
        max_tokens: None,
        cache_config: CacheConfig::default(),
        prompt_cache_key: None,
        model_config: None,
    }
}

#[tokio::test]
async fn summary_caps_reasoning_without_changing_normal_turn_or_output_budget() {
    for (normal, summary) in [
        (ThinkingLevel::Off, ThinkingLevel::Off),
        (ThinkingLevel::Minimal, ThinkingLevel::Minimal),
        (ThinkingLevel::Low, ThinkingLevel::Low),
        (ThinkingLevel::Medium, ThinkingLevel::Low),
        (ThinkingLevel::High, ThinkingLevel::Low),
        (ThinkingLevel::Xhigh, ThinkingLevel::Low),
        (ThinkingLevel::Max, ThinkingLevel::Low),
    ] {
        let provider = Arc::new(ProgressProvider {
            events: vec![],
            requests: Mutex::new(vec![]),
            generated: None,
            retained_sender: Mutex::new(None),
        });
        let ctx = context(provider.clone(), normal);
        let result = llm::summarize(input(), &ctx, 8000, CancellationToken::new()).await;
        assert!(result.is_ok());
        assert_eq!(ctx.thinking_level, normal);
        let requests = provider.requests.lock();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].thinking_level, summary);
        assert_eq!(requests[0].max_tokens, Some(8000));
    }
}

#[tokio::test]
async fn summary_consumes_live_deltas_and_resets_activity_for_split_turn() {
    let generated = Arc::new(Notify::new());
    let provider = Arc::new(ProgressProvider {
        events: vec![
            StreamEvent::Start,
            StreamEvent::TextDelta {
                content_index: 0,
                delta: String::new(),
            },
            StreamEvent::ThinkingDelta {
                content_index: 0,
                delta: "reasoning".into(),
            },
            StreamEvent::TextDelta {
                content_index: 1,
                delta: "partial text, not the final summary".into(),
            },
        ],
        requests: Mutex::new(vec![]),
        generated: Some(generated.clone()),
        retained_sender: Mutex::new(None),
    });
    let activities = Arc::new(Mutex::new(vec![]));
    let observed = activities.clone();
    let observer: CompactionObserver = Arc::new(move |update| {
        if let Some(activity) = update.activity {
            observed.lock().push(activity);
            if activity == CompactionActivity::Generating {
                generated.notify_one();
            }
        }
    });
    let mut input = input();
    input.turn_prefix = Some("[Assistant]: active turn".into());
    let ctx = context(provider.clone(), ThinkingLevel::High);
    // Test guard only: production compaction has no deadline.
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        llm::summarize_with_observer(input, &ctx, 8000, CancellationToken::new(), Some(observer)),
    )
    .await;
    let output = match output {
        Ok(Ok(output)) => output,
        result => panic!("live stream stalled or failed: {result:?}"),
    };
    assert!(output.summary.contains("authoritative summary"));
    assert!(!output.summary.contains("partial text"));
    assert_eq!(activities.lock().as_slice(), [
        CompactionActivity::Waiting,
        CompactionActivity::Generating,
        CompactionActivity::Waiting,
        CompactionActivity::Generating,
    ]);
    assert_eq!(provider.requests.lock().len(), 2);
    assert!(provider.retained_sender.lock().is_some());
}

#[tokio::test]
async fn start_and_empty_deltas_do_not_claim_summary_is_generating() {
    let provider = Arc::new(ProgressProvider {
        events: vec![StreamEvent::Start, StreamEvent::ThinkingDelta {
            content_index: 0,
            delta: String::new(),
        }],
        requests: Mutex::new(vec![]),
        generated: None,
        retained_sender: Mutex::new(None),
    });
    let activities = Arc::new(Mutex::new(vec![]));
    let observed = activities.clone();
    let observer: CompactionObserver = Arc::new(move |update| {
        if let Some(activity) = update.activity {
            observed.lock().push(activity);
        }
    });
    let result = llm::summarize_with_observer(
        input(),
        &context(provider, ThinkingLevel::High),
        8000,
        CancellationToken::new(),
        Some(observer),
    )
    .await;
    assert!(result.is_ok());
    assert_eq!(activities.lock().as_slice(), [CompactionActivity::Waiting]);
}
