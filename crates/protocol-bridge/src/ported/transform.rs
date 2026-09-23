use serde_json::Value;

pub fn is_openai_o_series(model: &str) -> bool {
    model.len() > 1
        && model.starts_with('o')
        && model
            .as_bytes()
            .get(1)
            .is_some_and(|byte| byte.is_ascii_digit())
}

pub fn supports_reasoning_effort(model: &str) -> bool {
    let normalized = model.to_ascii_lowercase();
    is_openai_o_series(&normalized)
        || normalized
            .strip_prefix("gpt-")
            .and_then(|rest| rest.chars().next())
            .is_some_and(|byte| byte.is_ascii_digit() && byte >= '5')
        || normalized.starts_with("grok-4.5")
        || normalized.starts_with("grok-build-")
}

pub fn inject_openai_stream_include_usage(value: &mut Value) {
    if value.get("stream").and_then(Value::as_bool) != Some(true) {
        return;
    }
    let Some(object) = value.as_object_mut() else {
        // `Value::get` currently makes the branch above imply an object, but
        // keep this boundary defensive: protocol conversion must reject or
        // ignore malformed JSON rather than panic the request worker if that
        // assumption changes in a future serde_json version/refactor.
        return;
    };
    let options = object
        .entry("stream_options")
        .or_insert_with(|| serde_json::json!({}));
    if let Some(options) = options.as_object_mut() {
        options.insert("include_usage".into(), Value::Bool(true));
    }
}
