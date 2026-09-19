//! Responses/Chat protocol bridge for the single OmniBridge provider.
//!
//! The `ported` modules are adapted from CC Switch v3.20.3 at the pinned
//! commit recorded in `THIRD_PARTY_NOTICES.md`.  They remain grouped by the
//! same responsibilities as the upstream implementation: tool context,
//! reasoning extraction, Responses SSE envelope, streaming state machine and
//! history completion.  The public wrapper below adds the local route and
//! security-domain contract used by OmniBridge.

use bytes::Bytes;
use futures::Stream;
use serde_json::{Value, json};
use std::io;
use std::sync::Arc;
use thiserror::Error;

pub mod json_canonical;
pub mod sse;
pub mod tool_media;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexChatReasoningConfig {
    pub supports_thinking: Option<bool>,
    pub supports_effort: Option<bool>,
    pub thinking_param: Option<String>,
    pub effort_param: Option<String>,
    pub effort_value_mode: Option<String>,
    pub output_format: Option<String>,
    pub effort_levels: Option<Vec<String>>,
}

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("request must be a JSON object")]
    NotObject,
    #[error("context boundary: {0}")]
    ContextBoundary(String),
    #[error("invalid upstream response: {0}")]
    InvalidResponse(String),
    #[error("invalid SSE stream: {0}")]
    InvalidSse(String),
    #[error("missing Chat Completions choice")]
    MissingChoice,
    #[error("unsupported protocol bridge operation: {0}")]
    Unsupported(String),
    #[error("protocol transformation failed: {0}")]
    TransformError(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteDomain {
    Official,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeContext {
    pub route_id: String,
    pub generation: u64,
    pub domain: RouteDomain,
    pub account_fingerprint: Option<String>,
    pub upstream_model: String,
}

impl BridgeContext {
    pub fn validate(&self) -> Result<(), BridgeError> {
        if self.route_id.trim().is_empty() || self.upstream_model.trim().is_empty() {
            return Err(BridgeError::ContextBoundary(
                "route id and upstream model are required".into(),
            ));
        }
        Ok(())
    }
}

pub mod ported {
    #[path = "codex_chat_common.rs"]
    pub(crate) mod codex_chat_common;
    #[path = "codex_chat_history.rs"]
    pub(crate) mod codex_chat_history;
    #[path = "codex_responses_sse.rs"]
    pub(crate) mod codex_responses_sse;
    #[path = "streaming_codex_chat.rs"]
    pub(crate) mod streaming_codex_chat;
    #[path = "transform.rs"]
    pub(crate) mod transform;
    #[path = "transform_codex_chat.rs"]
    pub(crate) mod transform_codex_chat;
}

pub use ported::codex_chat_history::CodexChatHistoryStore;
pub use ported::transform_codex_chat::responses_to_chat_completions_with_reasoning;

/// Convert a Responses request for a Chat-only upstream and bind the selected
/// upstream model.  The bridge never carries provider selection in the body;
/// `BridgeContext` is the authoritative route decision.
pub fn responses_to_chat(
    mut request: Value,
    context: &BridgeContext,
    reasoning: Option<&CodexChatReasoningConfig>,
) -> Result<Value, BridgeError> {
    context.validate()?;
    if context.domain != RouteDomain::Custom {
        return Err(BridgeError::ContextBoundary(
            "official requests cannot enter the Chat adapter".into(),
        ));
    }
    request
        .as_object_mut()
        .ok_or(BridgeError::NotObject)?
        .insert(
            "model".into(),
            Value::String(context.upstream_model.clone()),
        );
    let result = responses_to_chat_completions_with_reasoning(request, reasoning)
        .map_err(|error| BridgeError::ContextBoundary(error.to_string()))?;
    Ok(result)
}

/// The logical model id carried by the original client request.
///
/// The bridge rewrites `model` to the upstream id only on the request it
/// forwards upstream, so the untouched client request stays the authoritative
/// source for the model Codex actually asked for.
fn request_model(request: &Value) -> Option<String> {
    request
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
        .map(ToOwned::to_owned)
}

/// Convert one non-streaming Chat response to a real Responses response.
pub fn chat_to_responses(response: Value, context: &BridgeContext) -> Result<Value, BridgeError> {
    chat_to_responses_with_request(response, context, &Value::Null)
}

pub fn chat_to_responses_with_request(
    mut response: Value,
    context: &BridgeContext,
    request: &Value,
) -> Result<Value, BridgeError> {
    context.validate()?;
    if context.domain != RouteDomain::Custom {
        return Err(BridgeError::ContextBoundary(
            "official responses cannot be decoded as Chat".into(),
        ));
    }
    if !response.is_object() {
        return Err(BridgeError::InvalidResponse(
            "Chat response must be an object".into(),
        ));
    }
    // Echo the logical model the client asked for.  Overwriting this with the
    // upstream model leaked the provider's private model name back to Codex,
    // which correlates the reply against the model id it requested.  The
    // original client request still carries that id; fall back to the upstream
    // name only when the request has no model to offer.
    response["model"] =
        Value::String(request_model(request).unwrap_or_else(|| context.upstream_model.clone()));
    let tool_context = ported::transform_codex_chat::build_codex_tool_context_from_request(request);
    ported::transform_codex_chat::chat_completion_to_response_with_context(response, &tool_context)
        .map_err(|error| BridgeError::InvalidResponse(error.to_string()))
}

/// Convert a stream of Chat SSE bytes into a complete Responses SSE stream.
/// The ported state machine owns event ordering and tool-call accumulation;
/// the caller provides a bounded byte stream so cancellation can stop the
/// upstream request immediately.
pub async fn chat_sse_to_responses(
    input: Vec<bytes::Bytes>,
    context: &BridgeContext,
    request: &Value,
) -> Result<Vec<bytes::Bytes>, BridgeError> {
    context.validate()?;
    if context.domain != RouteDomain::Custom {
        return Err(BridgeError::ContextBoundary(
            "official responses cannot be decoded as Chat".into(),
        ));
    }
    let tool_context = ported::transform_codex_chat::build_codex_tool_context_from_request(request);
    let stream = futures::stream::iter(input.into_iter().map(Ok::<_, std::io::Error>));
    let converted = ported::streaming_codex_chat::create_responses_sse_stream_from_chat_with_model(
        Box::pin(stream),
        tool_context,
        request_model(request),
    );
    use futures::StreamExt;
    futures::pin_mut!(converted);
    let mut output = Vec::new();
    while let Some(item) = converted.next().await {
        output.push(item.map_err(|error| BridgeError::InvalidSse(error.to_string()))?);
    }
    Ok(output)
}

/// Streaming form of [`chat_sse_to_responses`].  The upstream byte stream is
/// consumed lazily by the ported state machine, so dropping the returned
/// stream also drops the reqwest body and cancels the upstream request.
pub fn chat_sse_to_responses_stream<S, E>(
    input: S,
    context: BridgeContext,
    request: Value,
) -> Result<impl Stream<Item = Result<Bytes, io::Error>> + Send, BridgeError>
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: std::error::Error + Send + 'static,
{
    context.validate()?;
    if context.domain != RouteDomain::Custom {
        return Err(BridgeError::ContextBoundary(
            "official responses cannot be decoded as Chat".into(),
        ));
    }
    let tool_context =
        ported::transform_codex_chat::build_codex_tool_context_from_request(&request);
    Ok(
        ported::streaming_codex_chat::create_responses_sse_stream_from_chat_with_model(
            input,
            tool_context,
            request_model(&request),
        ),
    )
}

/// Observe a Responses SSE stream for tool-call history while yielding the
/// original bytes unchanged.  The observer stores only portable call data;
/// it never persists opaque reasoning or official account material.
pub fn record_responses_sse_stream(
    input: impl Stream<Item = Result<Bytes, io::Error>> + Send + 'static,
    history: Arc<CodexChatHistoryStore>,
) -> impl Stream<Item = Result<Bytes, io::Error>> + Send {
    ported::codex_chat_history::record_responses_sse_stream(input, history)
}

pub fn record_responses_sse_stream_with_callback(
    input: impl Stream<Item = Result<Bytes, io::Error>> + Send + 'static,
    history: Arc<CodexChatHistoryStore>,
    callback: Arc<dyn Fn(&str) + Send + Sync>,
) -> impl Stream<Item = Result<Bytes, io::Error>> + Send {
    ported::codex_chat_history::record_responses_sse_stream_with_callback(
        input,
        history,
        Some(callback),
    )
}

/// Record the portable part of a non-streaming Responses response for later
/// tool-loop hydration.  The response object is scrubbed before it reaches
/// the history store; only call ids, names, arguments and portable summaries
/// remain.
pub async fn record_portable_response(history: &CodexChatHistoryStore, response: &Value) -> usize {
    let mut sanitized = response.clone();
    if let Some(output) = sanitized.get_mut("output").and_then(Value::as_array_mut) {
        output.retain(|item| {
            !matches!(
                item.get("type").and_then(Value::as_str),
                Some("web_search_call") | Some("computer_call") | Some("file_search_call")
            )
        });
        for item in output {
            if let Some(object) = item.as_object_mut() {
                match object.get("type").and_then(Value::as_str) {
                    Some("reasoning") => {
                        object.remove("id");
                        object.remove("encrypted_content");
                    }
                    Some("function_call") | Some("custom_tool_call") | Some("tool_search_call") => {
                        object.remove("id");
                        object.remove("status");
                    }
                    _ => {}
                }
            }
        }
    }
    history.record_response(&sanitized).await
}

/// Drop all route-private identifiers before a request crosses a security
/// domain.  This is intentionally structural instead of a string filter.
pub fn portable_request(request: &Value) -> Result<Value, BridgeError> {
    let mut request = request.clone();
    let object = request.as_object_mut().ok_or(BridgeError::NotObject)?;
    for key in [
        "previous_response_id",
        "conversation",
        "encrypted_content",
        "prompt_cache_key",
        "safety_identifier",
    ] {
        object.remove(key);
    }
    if let Some(input) = object.get_mut("input") {
        portable_items(input)?;
    }
    Ok(request)
}

fn portable_items(value: &mut Value) -> Result<(), BridgeError> {
    let Some(items) = value.as_array_mut() else {
        return Ok(());
    };
    for item in items.iter_mut() {
        let Some(object) = item.as_object_mut() else {
            continue;
        };
        match object.get("type").and_then(Value::as_str) {
            Some("reasoning") => {
                object.remove("id");
                object.remove("encrypted_content");
                if object
                    .get("summary")
                    .and_then(Value::as_array)
                    .is_none_or(|summary| summary.is_empty())
                {
                    return Err(BridgeError::ContextBoundary(
                        "opaque reasoning has no portable summary".into(),
                    ));
                }
            }
            Some("function_call") | Some("custom_tool_call") => {
                object.remove("id");
                object.remove("status");
            }
            Some("web_search_call") | Some("computer_call") | Some("file_search_call") => {
                return Err(BridgeError::ContextBoundary(
                    "hosted tool result cannot cross route domains".into(),
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Build a Responses error object without copying upstream bodies or secrets.
pub fn error_response(kind: &str, message: &str) -> Value {
    json!({
        "error": {
            "type": kind,
            "message": message,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> BridgeContext {
        BridgeContext {
            route_id: "custom:test".into(),
            generation: 7,
            domain: RouteDomain::Custom,
            account_fingerprint: None,
            upstream_model: "provider-model".into(),
        }
    }

    #[test]
    fn bridge_preserves_tool_context_and_changes_only_upstream_model() {
        let request = json!({
            "model": "custom/logical",
            "instructions": "be precise",
            "input": [{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}],
            "tools": [{"type":"function","name":"shell","parameters":{"type":"object"}}],
            "stream": true
        });
        let output = responses_to_chat(request, &context(), None).unwrap();
        assert_eq!(output["model"], "provider-model");
        assert_eq!(output["messages"][0]["role"], "system");
        assert_eq!(output["tools"][0]["function"]["name"], "shell");
        assert_eq!(output["stream"], true);
    }

    #[test]
    fn chat_to_responses_echoes_logical_model_instead_of_upstream_model() {
        let context = context();
        let request = json!({"model": "custom/logical"});
        let response = json!({
            "id": "chatcmpl_1",
            "object": "chat.completion",
            "model": "provider-model",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "hi"},
                "finish_reason": "stop"
            }]
        });

        let output = chat_to_responses_with_request(response, &context, &request).unwrap();
        assert_eq!(
            output["model"], "custom/logical",
            "Codex correlates on the logical model id it requested"
        );
    }

    #[tokio::test]
    async fn streaming_responses_echo_logical_model_instead_of_upstream_model() {
        let input = vec![Bytes::from_static(
            b"data: {\"id\":\"chatcmpl_1\",\"model\":\"provider-model\",\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        )];

        let output = chat_sse_to_responses(input, &context(), &json!({"model": "custom/logical"}))
            .await
            .unwrap();
        let text = String::from_utf8(output.concat()).unwrap();
        assert!(
            text.contains("\"model\":\"custom/logical\""),
            "streamed responses must echo the logical model id: {text}"
        );
        assert!(
            !text.contains("\"model\":\"provider-model\""),
            "streamed responses must not leak the upstream model id: {text}"
        );
    }

    #[test]
    fn portable_request_rejects_hosted_tool_and_removes_private_id() {
        let request = json!({
            "previous_response_id": "resp_private",
            "input": [{"type":"web_search_call","id":"call_private"}]
        });
        let error = portable_request(&request).unwrap_err();
        assert!(error.to_string().contains("hosted tool"));
        assert!(!error.to_string().contains("resp_private"));
    }
}
