//! Wire types for the engine ↔ host boundary.
//!
//! The engine core knows nothing about specific tools like `plan` or
//! `ask_user`. Instead, a host (the TypeScript CLI, via the NAPI addon)
//! registers tools by describing them with a [`HostToolSpec`] and answers
//! [`HostToolCall`]s with a [`HostToolResponse`]. All payloads are plain
//! serde types so they cross the NAPI boundary as JSON.

use serde::Deserialize;
use serde::Serialize;

use crate::types::Content;

/// Static description of a host-owned tool.
///
/// The host supplies everything the engine needs to advertise the tool to the
/// LLM and route calls back — the actual execution logic lives in the host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostToolSpec {
    /// Canonical tool name used in LLM tool_use.
    pub name: String,
    /// Human-readable label for UI.
    pub label: String,
    /// Description shown to the LLM.
    pub description: String,
    /// JSON Schema for the tool parameters.
    pub parameters_schema: serde_json::Value,
    /// Optional one-line capability summary for the system prompt tool list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_snippet: Option<String>,
    /// Model-specific name aliases: `(model_pattern, llm_name)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub name_aliases: Vec<(String, String)>,
    /// A successful call ends the run without a follow-up model response.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ends_turn: bool,
}

/// A single invocation of a host tool, forwarded from the engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostToolCall {
    /// The tool being invoked (canonical name).
    pub tool_name: String,
    /// The ID of this tool call, for correlation.
    pub tool_call_id: String,
    /// Arguments the LLM passed, already alias-normalized and schema-coerced.
    pub arguments: serde_json::Value,
}

/// The host's answer to a [`HostToolCall`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostToolResponse {
    /// Content blocks returned to the LLM as the tool result.
    pub content: Vec<Content>,
    /// Structured metadata for UI rendering and state reconstruction.
    /// Never sent to the LLM.
    #[serde(default)]
    pub details: serde_json::Value,
    /// Whether this result represents a tool error.
    #[serde(default)]
    pub is_error: bool,
}

impl HostToolResponse {
    /// Convenience constructor for a plain-text success result.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![Content::Text { text: text.into() }],
            details: serde_json::Value::Null,
            is_error: false,
        }
    }
}
