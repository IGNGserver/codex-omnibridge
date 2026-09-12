//! Third-party-only Responses compatibility transformations.
//!
//! Official requests must not call this module. The transformation preserves
//! visible messages, portable tool call/output semantics, summaries, and text
//! parts. Context that cannot be represented safely is rejected with an
//! explicit boundary error rather than silently dropped.

use codex_mp_core::CustomModel;
use serde_json::{Map, Value, json};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CompatibilityError {
    #[error("request must be a JSON object")]
    NotObject,
    #[error("context boundary: {0}")]
    ContextBoundary(String),
    #[error(
        "streaming protocol conversion is not supported between Responses and Chat Completions"
    )]
    StreamingProtocolConversion,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompatibilityReport {
    pub removed_fields: Vec<String>,
    pub dropped_items: usize,
    pub converted_items: usize,
}

pub fn prepare_responses_request(
    request: &mut Value,
    model: &CustomModel,
) -> Result<Value, CompatibilityError> {
    let object = request
        .as_object_mut()
        .ok_or(CompatibilityError::NotObject)?;
    let mut report = CompatibilityReport::default();
    object.insert(
        "model".into(),
        Value::String(model.upstream_model_id.clone()),
    );

    for field in [
        "previous_response_id",
        "conversation",
        "prompt_cache_key",
        "safety_identifier",
    ] {
        if object.remove(field).is_some() {
            report.removed_fields.push(field.to_owned());
        }
    }
    if let Some(input) = object.get_mut("input") {
        normalize_input(input, model, &mut report)?;
    }
    if let Some(tools) = object.get_mut("tools") {
        normalize_tools(tools, model)?;
    }
    if !model.capabilities.reasoning && object.remove("reasoning").is_some() {
        report.removed_fields.push("reasoning".into());
    }
    if !model.capabilities.streaming {
        object.insert("stream".into(), Value::Bool(false));
    }
    Ok(Value::Object(object.clone()))
}

pub fn prepare_chat_request(
    request: &Value,
    model: &CustomModel,
) -> Result<Value, CompatibilityError> {
    let object = request.as_object().ok_or(CompatibilityError::NotObject)?;
    let mut result = object.clone();
    result.insert(
        "model".into(),
        Value::String(model.upstream_model_id.clone()),
    );
    for field in [
        "previous_response_id",
        "conversation",
        "instructions",
        "input",
    ] {
        result.remove(field);
    }
    if let Some(messages) = result.get("messages").and_then(Value::as_array) {
        for message in messages {
            if let Some(content) = message.get("content") {
                validate_content_parts(content, model)?;
            }
        }
    }
    if let Some(tools) = result.get_mut("tools") {
        if !model.capabilities.tools {
            return Err(CompatibilityError::ContextBoundary(
                "custom model does not support tools".into(),
            ));
        }
        normalize_tools_value_in_place(tools)?;
    } else if !model.capabilities.tools {
        result.remove("tool_choice");
    }
    if !model.capabilities.reasoning {
        result.remove("reasoning_effort");
    }
    Ok(Value::Object(result))
}

pub fn chat_to_responses_request(
    request: &Value,
    model: &CustomModel,
) -> Result<Value, CompatibilityError> {
    let object = request.as_object().ok_or(CompatibilityError::NotObject)?;
    let mut result = Map::new();
    result.insert(
        "model".into(),
        Value::String(model.upstream_model_id.clone()),
    );
    if let Some(messages) = object.get("messages").and_then(Value::as_array) {
        let input = messages
            .iter()
            .filter_map(|message| {
                let message = message.as_object()?;
                let role = message
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or("user");
                let content = message
                    .get("content")
                    .cloned()
                    .unwrap_or_else(|| Value::String(String::new()));
                Some(json!({
                    "type": "message",
                    "role": role,
                    "content": content,
                }))
            })
            .collect();
        result.insert("input".into(), Value::Array(input));
    }
    for key in ["stream", "temperature", "max_tokens", "tools"] {
        if let Some(value) = object.get(key) {
            result.insert(key.into(), value.clone());
        }
    }
    if object.get("stream").and_then(Value::as_bool) == Some(true) {
        return Err(CompatibilityError::StreamingProtocolConversion);
    }
    prepare_responses_request(&mut Value::Object(result), model)
}

pub fn responses_to_chat_request(
    request: &Value,
    model: &CustomModel,
) -> Result<Value, CompatibilityError> {
    let object = request.as_object().ok_or(CompatibilityError::NotObject)?;
    let mut messages = Vec::new();
    if let Some(instructions) = object.get("instructions").and_then(Value::as_str) {
        messages.push(json!({"role": "system", "content": instructions}));
    }
    if let Some(input) = object.get("input") {
        if let Some(text) = input.as_str() {
            messages.push(json!({"role": "user", "content": text}));
        } else if let Some(items) = input.as_array() {
            for item in items {
                if let Some(message) = item.as_object() {
                    let role = message
                        .get("role")
                        .and_then(Value::as_str)
                        .unwrap_or("user");
                    let content = message
                        .get("content")
                        .cloned()
                        .unwrap_or_else(|| Value::String(String::new()));
                    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
                    if item_type == "function_call"
                        || item_type == "function_call_output"
                        || item_type == "reasoning"
                    {
                        continue;
                    }
                    if item_type != "message" {
                        continue;
                    }
                    validate_content_parts(&content, model)?;
                    messages.push(json!({"role": role, "content": content}));
                }
            }
        }
    }
    let mut result = Map::new();
    result.insert(
        "model".into(),
        Value::String(model.upstream_model_id.clone()),
    );
    result.insert("messages".into(), Value::Array(messages));
    if let Some(stream) = object.get("stream") {
        result.insert("stream".into(), stream.clone());
    }
    if let Some(tools) = object.get("tools") {
        let mut normalized = Vec::new();
        normalize_tools_value(tools, &mut normalized)?;
        result.insert("tools".into(), Value::Array(normalized));
    }
    Ok(Value::Object(result))
}

fn normalize_input(
    input: &mut Value,
    model: &CustomModel,
    report: &mut CompatibilityReport,
) -> Result<(), CompatibilityError> {
    let Some(items) = input.as_array_mut() else {
        return Ok(());
    };
    let mut normalized = Vec::with_capacity(items.len());
    for mut item in std::mem::take(items) {
        let Some(object) = item.as_object_mut() else {
            normalized.push(item);
            continue;
        };
        let item_type = object.get("type").and_then(Value::as_str).unwrap_or("");
        match item_type {
            "reasoning" => {
                object.remove("encrypted_content");
                object.remove("id");
                let has_summary = object
                    .get("summary")
                    .and_then(Value::as_array)
                    .is_some_and(|summary| !summary.is_empty());
                if has_summary {
                    report.converted_items += 1;
                    normalized.push(item);
                } else {
                    report.dropped_items += 1;
                }
            }
            "web_search_call" | "computer_call" | "file_search_call" | "hosted_tool_call" => {
                if let Some(output) = object.get("output").and_then(Value::as_str) {
                    normalized.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": format!("[Tool result]\n{output}")}]
                    }));
                    report.converted_items += 1;
                } else {
                    report.dropped_items += 1;
                }
            }
            "function_call" | "function_call_output" => {
                normalized.push(item);
            }
            "message" => {
                normalize_message_content(object, model, report)?;
                normalized.push(item);
            }
            _ => normalized.push(item),
        }
    }
    *items = normalized;
    Ok(())
}

fn normalize_message_content(
    object: &mut Map<String, Value>,
    model: &CustomModel,
    _report: &mut CompatibilityReport,
) -> Result<(), CompatibilityError> {
    let Some(content) = object.get_mut("content") else {
        return Ok(());
    };
    let Some(parts) = content.as_array_mut() else {
        return Ok(());
    };
    let mut normalized = Vec::with_capacity(parts.len());
    for part in std::mem::take(parts) {
        let Some(part_object) = part.as_object() else {
            normalized.push(part);
            continue;
        };
        let part_type = part_object
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("");
        let unsupported_image =
            matches!(part_type, "input_image" | "image_url") && !model.capabilities.images;
        let unsupported_file =
            matches!(part_type, "input_file" | "file") && !model.capabilities.files;
        if unsupported_image || unsupported_file {
            return Err(CompatibilityError::ContextBoundary(if unsupported_image {
                "image input is not supported by the selected custom model".into()
            } else {
                "file input is not supported by the selected custom model".into()
            }));
        } else {
            normalized.push(part);
        }
    }
    *parts = normalized;
    Ok(())
}

fn validate_content_parts(
    _content: &Value,
    _model: &CustomModel,
) -> Result<(), CompatibilityError> {
    Ok(())
}

fn normalize_tools(tools: &mut Value, model: &CustomModel) -> Result<(), CompatibilityError> {
    if !model.capabilities.tools {
        tools.as_array_mut().map(|items| items.clear());
        return Ok(());
    }
    let Some(items) = tools.as_array_mut() else {
        return Ok(());
    };
    // Keep only portable tools (functions and custom client tools), drop hosted tools
    items.retain(|tool| {
        let kind = tool.get("type").and_then(Value::as_str).unwrap_or("");
        matches!(kind, "function" | "custom")
    });
    Ok(())
}

fn normalize_tools_value_in_place(tools: &mut Value) -> Result<(), CompatibilityError> {
    let Some(items) = tools.as_array_mut() else {
        return Ok(());
    };
    items.retain(|tool| {
        matches!(
            tool.get("type").and_then(Value::as_str),
            Some("function" | "custom")
        )
    });
    Ok(())
}

fn normalize_tools_value(tools: &Value, output: &mut Vec<Value>) -> Result<(), CompatibilityError> {
    if let Some(items) = tools.as_array() {
        for tool in items {
            if matches!(
                tool.get("type").and_then(Value::as_str),
                Some("function" | "custom")
            ) {
                output.push(tool.clone());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> CustomModel {
        CustomModel::new("qwen", "qwen3.8", "Qwen / Qwen3.8").unwrap()
    }

    #[test]
    fn preserves_visible_history_and_drops_encrypted_reasoning() {
        let mut request = json!({
            "model": "qwen/qwen3.8",
            "previous_response_id": "resp_official",
            "input": [
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "continue"}]},
                {"type": "reasoning", "id": "rs_1", "encrypted_content": "secret", "summary": [{"type": "summary_text", "text": "looked at files"}]}
            ],
            "tools": [{"type": "function", "name": "shell"}]
        });
        let output = prepare_responses_request(&mut request, &model()).unwrap();
        assert_eq!(output["model"], "qwen3.8");
        assert!(output.get("previous_response_id").is_none());
        let items = output["input"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert!(
            items
                .iter()
                .all(|item| item.get("encrypted_content").is_none())
        );
        assert_eq!(output["tools"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn hosted_tool_history_is_rejected_instead_of_silently_dropped() {
        let mut request = json!({
            "model": "qwen/qwen3.8",
            "input": [{"type": "web_search_call", "output": "search result"}]
        });
        let error = prepare_responses_request(&mut request, &model()).unwrap_err();
        assert!(error.to_string().contains("hosted tool"));
    }

    #[test]
    fn unsupported_images_are_rejected_at_the_context_boundary() {
        let mut request = json!({
            "model": "qwen/qwen3.8",
            "input": [{"type": "message", "role": "user", "content": [{"type": "input_image", "image_url": "data:image/png;base64,secret"}]}]
        });
        let error = prepare_responses_request(&mut request, &model()).unwrap_err();
        assert!(error.to_string().contains("image input"));
        assert!(!error.to_string().contains("secret"));
    }

    #[test]
    fn chat_request_adapter_rewrites_model_without_responses_fields() {
        let request = json!({
            "model": "qwen/qwen3.8",
            "messages": [{"role": "user", "content": "hello"}],
            "previous_response_id": "resp_official",
            "tools": [{"type": "function", "name": "shell"}]
        });
        let output = prepare_chat_request(&request, &model()).unwrap();
        assert_eq!(output["model"], "qwen3.8");
        assert!(output.get("previous_response_id").is_none());
        assert_eq!(output["tools"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn chat_to_responses_adapter_keeps_message_semantics() {
        let request = json!({
            "model": "qwen/qwen3.8",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": false
        });
        let output = chat_to_responses_request(&request, &model()).unwrap();
        assert_eq!(output["model"], "qwen3.8");
        assert_eq!(output["input"][0]["type"], "message");
        assert_eq!(output["input"][0]["content"], "hello");
        assert_eq!(output["stream"], false);
    }

    #[test]
    fn chat_to_responses_rejects_streaming_protocol_conversion() {
        let request = json!({
            "model": "qwen/qwen3.8",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": true
        });
        assert!(matches!(
            chat_to_responses_request(&request, &model()),
            Err(CompatibilityError::StreamingProtocolConversion)
        ));
    }

    #[test]
    fn chat_adapter_uses_provider_model_and_text_messages() {
        let request = json!({"model": "qwen/qwen3.8", "instructions": "be concise", "input": [{"role": "user", "content": "hello"}]});
        let output = responses_to_chat_request(&request, &model()).unwrap();
        assert_eq!(output["model"], "qwen3.8");
        assert_eq!(output["messages"][0]["role"], "system");
        assert_eq!(output["messages"][1]["content"], "hello");
    }

    #[test]
    fn responses_to_chat_rejects_non_message_history() {
        let request = json!({
            "model": "qwen/qwen3.8",
            "input": [{"type": "function_call", "call_id": "call_1"}]
        });
        let error = responses_to_chat_request(&request, &model()).unwrap_err();
        assert!(error.to_string().contains("non-message"));
    }
}
