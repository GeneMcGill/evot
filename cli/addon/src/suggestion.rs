use std::sync::Mutex;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// A disposable side request, independent of the foreground Run event stream.
#[napi]
pub struct NapiSuggestion {
    handle: Mutex<Option<JoinHandle<Option<String>>>>,
    cancel: CancellationToken,
}

impl NapiSuggestion {
    pub fn start(
        messages: Vec<evot::suggestion::SuggestionMessage>,
        product: Option<evot::suggestion::SuggestionProductContext>,
    ) -> Self {
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let handle = tokio::spawn(evot::suggestion::generate_with_context(
            messages,
            product,
            task_cancel,
        ));
        Self {
            handle: Mutex::new(Some(handle)),
            cancel,
        }
    }
}

impl Drop for NapiSuggestion {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[napi]
impl NapiSuggestion {
    #[napi]
    pub async fn result(&self) -> Result<Option<String>> {
        let handle = self
            .handle
            .lock()
            .map_err(|_| Error::from_reason("suggestion task lock poisoned"))?
            .take();
        match handle {
            Some(handle) => Ok(handle.await.ok().flatten()),
            None => Ok(None),
        }
    }

    #[napi]
    pub fn abort(&self) {
        self.cancel.cancel();
    }
}
