use std::sync::Arc;
use std::sync::Mutex;

use evot::api::Agent;
use evot_engine::context::compaction::types::CompactionActivity;
use evot_engine::context::compaction::types::CompactionUpdate;
use napi::bindgen_prelude::*;
use napi_derive::napi;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[napi]
pub struct NapiCompaction {
    handle: Mutex<Option<JoinHandle<std::result::Result<String, String>>>>,
    cancel: CancellationToken,
    status: Arc<Mutex<CompactionUpdate>>,
}

impl NapiCompaction {
    pub fn start(
        agent: Arc<Agent>,
        session_id: String,
        custom_instructions: Option<String>,
    ) -> Self {
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let status = Arc::new(Mutex::new(CompactionUpdate {
            phase: evot::api::ManualCompactionPhase::Planning,
            activity: None,
        }));
        let observer_status = Arc::clone(&status);
        let observer: evot::api::ManualCompactionObserver = Arc::new(move |next| {
            if let Ok(mut current) = observer_status.lock() {
                *current = next;
            }
        });
        let handle = tokio::spawn(async move {
            let outcome = agent
                .compact_with_observer(
                    &session_id,
                    custom_instructions,
                    task_cancel,
                    Some(observer),
                )
                .await
                .map_err(|error| error.to_string())?;
            serde_json::to_string(&outcome)
                .map_err(|error| format!("serialize compaction: {error}"))
        });
        Self {
            handle: Mutex::new(Some(handle)),
            cancel,
            status,
        }
    }
}

#[napi]
impl NapiCompaction {
    #[napi]
    pub async fn result(&self) -> Result<String> {
        let handle = {
            let mut guard = self
                .handle
                .lock()
                .map_err(|_| Error::from_reason("compaction task lock poisoned"))?;
            guard
                .take()
                .ok_or_else(|| Error::from_reason("compaction result already consumed"))?
        };
        handle
            .await
            .map_err(|error| Error::from_reason(format!("compaction task failed: {error}")))?
            .map_err(Error::from_reason)
    }

    #[napi(getter)]
    pub fn phase(&self) -> Result<String> {
        self.status
            .lock()
            .map(|status| {
                match status.phase {
                    evot::api::ManualCompactionPhase::Pruning => "pruning",
                    evot::api::ManualCompactionPhase::Planning => "planning",
                    evot::api::ManualCompactionPhase::Remote => "remote",
                    evot::api::ManualCompactionPhase::LocalFallback => "local_fallback",
                    evot::api::ManualCompactionPhase::Local => "local",
                    evot::api::ManualCompactionPhase::Complete => "complete",
                }
                .to_string()
            })
            .map_err(|_| Error::from_reason("compaction status lock poisoned"))
    }

    #[napi(getter)]
    pub fn activity(&self) -> Result<Option<String>> {
        self.status
            .lock()
            .map(|status| {
                status.activity.map(|activity| {
                    match activity {
                        CompactionActivity::Waiting => "waiting",
                        CompactionActivity::Generating => "generating",
                    }
                    .to_string()
                })
            })
            .map_err(|_| Error::from_reason("compaction status lock poisoned"))
    }

    #[napi]
    pub fn abort(&self) {
        self.cancel.cancel();
    }
}
