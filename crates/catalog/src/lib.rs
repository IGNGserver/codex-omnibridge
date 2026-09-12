//! Codex model catalog discovery and schema-preserving merge.
//!
//! The official catalog is sourced from the installed Codex binary rather than
//! a GitHub `main` snapshot. Unknown fields are retained verbatim by merging
//! JSON values instead of deserializing into a local approximation of Codex's
//! fast-moving schema.

use codex_mp_core::{
    CustomModel, ProviderRegistry, atomic_replace, command_for_executable, set_private_permissions,
};
use serde_json::{Map, Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("Codex binary `{0}` was not found or could not be executed: {1}")]
    CodexCommand(String, String),
    #[error("Codex returned non-JSON model catalog: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("model catalog must be a JSON object containing a `models` array")]
    InvalidShape,
    #[error("catalog IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("custom model `{0}` conflicts with an official model")]
    OfficialConflict(String),
    #[error("custom model `{0}` has no usable catalog template")]
    NoTemplate(String),
}

pub fn discover_official_catalog(codex_bin: impl AsRef<Path>) -> Result<Value, CatalogError> {
    let requested_binary = codex_bin.as_ref();
    let mut command = command_for_executable(requested_binary);
    let output = command
        .args(["debug", "models", "--bundled"])
        .output()
        .map_err(|error| {
            CatalogError::CodexCommand(requested_binary.display().to_string(), error.to_string())
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CatalogError::CodexCommand(
            requested_binary.display().to_string(),
            format!("exit status {}; {}", output.status, stderr.trim()),
        ));
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

pub fn validate_catalog(catalog: &Value) -> Result<(), CatalogError> {
    let object = catalog.as_object().ok_or(CatalogError::InvalidShape)?;
    let models = object
        .get("models")
        .and_then(Value::as_array)
        .ok_or(CatalogError::InvalidShape)?;
    if models.iter().any(|model| !model.is_object()) {
        return Err(CatalogError::InvalidShape);
    }
    Ok(())
}

pub fn merge_catalog(official: &Value, registry: &ProviderRegistry) -> Result<Value, CatalogError> {
    validate_catalog(official)?;
    let mut merged = official.clone();
    let object = merged.as_object_mut().ok_or(CatalogError::InvalidShape)?;
    let models = object
        .get_mut("models")
        .and_then(Value::as_array_mut)
        .ok_or(CatalogError::InvalidShape)?;

    let official_slugs: std::collections::HashSet<String> = models
        .iter()
        .filter_map(|model| model.get("slug").and_then(Value::as_str).map(str::to_owned))
        .collect();
    let template = models
        .iter()
        .find(|model| model.is_object())
        .cloned()
        .ok_or_else(|| CatalogError::NoTemplate("all custom models".into()))?;

    for (provider, model) in registry.enabled_custom_models() {
        if official_slugs.contains(&model.logical_model_id) {
            return Err(CatalogError::OfficialConflict(
                model.logical_model_id.clone(),
            ));
        }
        models.push(custom_entry(&template, provider.name.as_str(), model));
    }
    Ok(merged)
}

fn custom_entry(template: &Value, provider_name: &str, model: &CustomModel) -> Value {
    let mut entry = template.clone();
    let object = entry
        .as_object_mut()
        .expect("validated Codex model entries are objects");

    set_string(object, "slug", &model.logical_model_id);
    let display_name = if model.display_name.is_empty() {
        format!("{provider_name} / {}", model.upstream_model_id)
    } else {
        model.display_name.clone()
    };
    set_string(object, "display_name", &display_name);
    set_string(
        object,
        "description",
        &format!("Custom model routed by Codex MultiProvider to {provider_name}."),
    );
    object.insert("visibility".into(), Value::String("list".into()));
    object.insert("supported_in_api".into(), Value::Bool(true));
    object.insert("priority".into(), json!(1000));
    object.insert("upgrade".into(), Value::Null);
    object.insert("availability_nux".into(), Value::Null);
    object.insert("additional_speed_tiers".into(), Value::Array(Vec::new()));
    object.insert("service_tiers".into(), Value::Array(Vec::new()));
    object.insert("default_service_tier".into(), Value::Null);
    object.insert("supports_search_tool".into(), Value::Bool(false));
    object.insert("supports_experimental_context".into(), Value::Bool(false));
    object.insert("use_responses_lite".into(), Value::Bool(false));
    object.insert("include_apps_usage_instructions".into(), Value::Bool(false));
    object.insert(
        "include_plugin_usage_instructions".into(),
        Value::Bool(false),
    );
    object.insert(
        "include_skills_usage_instructions".into(),
        Value::Bool(false),
    );
    object.insert("input_modalities".into(), {
        let mut modalities = vec![Value::String("text".into())];
        if model.capabilities.images {
            modalities.push(Value::String("image".into()));
        }
        Value::Array(modalities)
    });
    if let Some(context_window) = model.context_window {
        object.insert("context_window".into(), json!(context_window));
        object.insert("max_context_window".into(), json!(context_window));
    }
    if !model.reasoning_levels.is_empty() {
        let levels = model
            .reasoning_levels
            .iter()
            .map(|effort| json!({ "effort": effort, "description": format!("{effort} reasoning") }))
            .collect();
        object.insert("supported_reasoning_levels".into(), Value::Array(levels));
        object.insert(
            "default_reasoning_level".into(),
            Value::String(model.reasoning_levels[0].clone()),
        );
    } else {
        object.remove("supported_reasoning_levels");
        object.remove("default_reasoning_level");
    }
    if !model.capabilities.reasoning {
        object.remove("supports_reasoning_summaries");
    }
    entry
}

fn set_string(object: &mut Map<String, Value>, key: &str, value: &str) {
    object.insert(key.to_owned(), Value::String(value.to_owned()));
}

pub fn write_catalog_atomic(catalog: &Value, path: impl AsRef<Path>) -> Result<(), CatalogError> {
    validate_catalog(catalog)?;
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, serde_json::to_vec_pretty(catalog)?)?;
    atomic_replace(temp, path)?;
    set_private_permissions(path)?;
    Ok(())
}

pub fn default_catalog_path() -> PathBuf {
    codex_mp_core::default_registry_path()
        .parent()
        .map(|path| path.join("models.json"))
        .unwrap_or_else(|| PathBuf::from("models.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_mp_core::{CustomModel, ProviderConfig, ProviderRegistry};
    use tempfile::tempdir;

    fn official() -> Value {
        json!({
            "models": [{
                "slug": "gpt-5.6-sol",
                "display_name": "GPT-5.6-Sol",
                "description": "official",
                "default_reasoning_level": "low",
                "supported_reasoning_levels": [{"effort": "low", "description": "low"}],
                "shell_type": "shell_command",
                "visibility": "list",
                "supported_in_api": true,
                "priority": 1,
                "input_modalities": ["text", "image"],
                "unknown_future_field": {"keep": true}
            }],
            "future_top_level": "keep"
        })
    }

    #[test]
    fn merge_keeps_official_and_unknown_fields() {
        let dir = tempdir().unwrap();
        let mut registry = ProviderRegistry::empty(dir.path().join("providers.json"));
        registry
            .add_provider(ProviderConfig::new("NewAPI", "https://example.test/v1").unwrap())
            .unwrap();
        registry
            .add_model(CustomModel::new("newapi", "qwen3.8", "NewAPI / Qwen3.8").unwrap())
            .unwrap();
        let merged = merge_catalog(&official(), &registry).unwrap();
        assert_eq!(merged["future_top_level"], "keep");
        assert_eq!(merged["models"][0]["slug"], "gpt-5.6-sol");
        assert_eq!(merged["models"][0]["unknown_future_field"]["keep"], true);
        assert_eq!(merged["models"][1]["slug"], "newapi/qwen3.8");
        assert_eq!(merged["models"][1]["display_name"], "NewAPI / Qwen3.8");
        assert!(merged["models"][1].get("unknown_future_field").is_none());
        assert!(merged["models"][1].get("shell_type").is_none());
    }

    #[test]
    fn command_output_is_current_installation_source() {
        let Some(codex_bin) = std::env::var_os("CODEX_MP_TEST_CODEX_BIN").map(PathBuf::from) else {
            return;
        };
        if !codex_bin.is_file() {
            return;
        }
        let catalog = discover_official_catalog(codex_bin).unwrap();
        validate_catalog(&catalog).unwrap();
        assert!(
            catalog["models"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["slug"] == "gpt-5.6-sol")
        );
    }

    #[test]
    fn real_catalog_snapshot_round_trips_with_custom_entry() {
        let path = std::env::var_os("CODEX_MP_CATALOG_FIXTURE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp/codex-mp-research/models.json"));
        if !path.exists() {
            return;
        }
        let official: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        validate_catalog(&official).unwrap();
        let dir = tempdir().unwrap();
        let mut registry = ProviderRegistry::empty(dir.path().join("providers.json"));
        registry
            .add_provider(ProviderConfig::new("NewAPI", "https://example.test/v1").unwrap())
            .unwrap();
        registry
            .add_model(CustomModel::new("newapi", "qwen3.8", "NewAPI / Qwen3.8").unwrap())
            .unwrap();
        let merged = merge_catalog(&official, &registry).unwrap();
        let encoded = serde_json::to_vec(&merged).unwrap();
        let reparsed: Value = serde_json::from_slice(&encoded).unwrap();
        validate_catalog(&reparsed).unwrap();
        assert_eq!(reparsed["models"].as_array().unwrap().len(), 9);
        assert_eq!(reparsed["models"][8]["slug"], "newapi/qwen3.8");
        assert_eq!(reparsed["models"][0]["slug"], "gpt-5.6-sol");
    }
}
