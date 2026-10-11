use std::sync::Arc;

use super::run::runtime;
use super::run::runtime::TurnFactory;
use super::tools::HostTools;
use super::tools::ToolMode;
use super::turn_assembler::TurnAssembler;
use super::turn_assembler::TurnBuildRequest;
use crate::conf::LlmConfig;
use crate::error::Result;
use crate::sessions::Session;

pub(super) struct AgentTurnFactory {
    pub assembler: Arc<TurnAssembler>,
    pub session: Arc<Session>,
    pub mode: ToolMode,
    pub session_id: String,
    pub llm: LlmConfig,
    /// Selection replaced by the latest model switch, for compaction.
    pub previous_llm: Option<LlmConfig>,
    pub host_tools: Option<HostTools>,
}

#[async_trait::async_trait]
impl TurnFactory for AgentTurnFactory {
    async fn build(&self, input: Vec<evot_engine::Content>) -> Result<runtime::TurnInput> {
        self.assembler
            .build_turn(
                &self.llm,
                self.mode,
                Arc::clone(&self.session),
                &self.session_id,
                TurnBuildRequest {
                    input,
                    host_tools: self.host_tools.clone(),
                    consume_process_notifications: true,
                    previous_llm: self.previous_llm.clone(),
                },
            )
            .await
    }
}
