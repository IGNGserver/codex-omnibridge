//! Codex model catalog discovery and schema-preserving merge.
//!
//! The official catalog is sourced from the installed Codex binary rather than
//! a GitHub `main` snapshot. Unknown fields are retained verbatim by merging
//! JSON values instead of deserializing into a local approximation of Codex's
//! fast-moving schema.

use codex_mp_core::{CustomModel, ProviderRegistry, command_for_executable};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Deadline for reading the bundled catalog out of the Codex binary.
const CATALOG_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(30);
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
    #[error("catalog model `{0}` does not match the stock Codex compatibility profile: {1}")]
    UnsupportedSchema(String, String),
}

pub fn discover_official_catalog(codex_bin: impl AsRef<Path>) -> Result<Value, CatalogError> {
    let requested_binary = codex_bin.as_ref();

    let run = |bundled: bool| -> Result<Value, CatalogError> {
        let mut command = command_for_executable(requested_binary);
        command.arg("debug").arg("models");
        if bundled {
            command.arg("--bundled");
        }
        // Bounded: a wrapper or wedged install that never exits used to hang
        // `codex-mp sync` for ever. Catalog discovery is required, so a timeout is
        // a real error here (unlike the optional version probe).
        let output = codex_mp_core::run_with_timeout(&mut command, CATALOG_DISCOVERY_TIMEOUT)
            .map_err(|error| {
                CatalogError::CodexCommand(
                    requested_binary.display().to_string(),
                    error.to_string(),
                )
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(CatalogError::CodexCommand(
                requested_binary.display().to_string(),
                format!("exit status {}; {}", output.status, stderr.trim()),
            ));
        }
        Ok(serde_json::from_slice(&output.stdout)?)
    };

    // `--bundled` asks Codex for the catalog compiled into the binary, which is
    // the authoritative list. A Codex build that predates the flag rejects it as
    // an unknown argument, and failing outright would make `sync` unusable on
    // that version — so fall back to the plain form. Both are verified to return
    // the same catalog on the versions available here.
    match run(true) {
        Ok(catalog) => Ok(catalog),
        Err(bundled_error) => match run(false) {
            Ok(catalog) => {
                eprintln!(
                    "codex-mp: `codex debug models --bundled` is unavailable on this Codex \
                     build; used `codex debug models` instead"
                );
                Ok(catalog)
            }
            // Report the original failure: it names the flag that was rejected and
            // is the more actionable of the two.
            Err(_) => Err(bundled_error),
        },
    }
}

pub fn validate_catalog(catalog: &Value) -> Result<(), CatalogError> {
    let object = catalog.as_object().ok_or(CatalogError::InvalidShape)?;
    let models = object
        .get("models")
        .and_then(Value::as_array)
        .ok_or(CatalogError::InvalidShape)?;
    // The checks below mirror Codex's own catalog schema. Presence alone is not
    // enough: `{"shell_type": null}` has the key but is rejected with
    // `invalid type: null, expected string or map`. Because stock Codex discards
    // the **entire** catalog on the first invalid entry (and then silently uses
    // its built-in models, hiding every custom model), validating only for
    // presence let a real P0 ship: three fields were emitted as `null`.
    //
    // Every rule here was confirmed by feeding candidate catalogs to the real
    // app-server; the observed message is quoted next to each check.
    let slug_of = |model_object: &serde_json::Map<String, Value>| {
        model_object
            .get("slug")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>")
            .to_owned()
    };
    for model in models {
        let Some(model_object) = model.as_object() else {
            return Err(CatalogError::InvalidShape);
        };
        let slug = slug_of(model_object);

        // `missing field ...` / `invalid type: null, expected string or map`.
        match model_object.get("shell_type") {
            Some(Value::Null) | None => {
                return Err(CatalogError::UnsupportedSchema(
                    slug,
                    "`shell_type` must be a string or map".into(),
                ));
            }
            Some(value) if !value.is_string() && !value.is_object() => {
                return Err(CatalogError::UnsupportedSchema(
                    slug,
                    "`shell_type` must be a string or map".into(),
                ));
            }
            Some(_) => {}
        }

        if model_object
            .get("slug")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            return Err(CatalogError::UnsupportedSchema(
                slug,
                "`slug` must be a non-empty string".into(),
            ));
        }

        // `missing field supported_reasoning_levels` / `expected a sequence`.
        match model_object.get("supported_reasoning_levels") {
            Some(Value::Array(_)) => {}
            _ => {
                return Err(CatalogError::UnsupportedSchema(
                    slug,
                    "`supported_reasoning_levels` must be present and be an array".into(),
                ));
            }
        }

        // `invalid type: null, expected a boolean`.
        match model_object.get("supports_search_tool") {
            Some(Value::Bool(_)) => {}
            _ => {
                return Err(CatalogError::UnsupportedSchema(
                    slug,
                    "`supports_search_tool` must be present and be a boolean".into(),
                ));
            }
        }

        // `invalid type: null, expected a sequence`.
        match model_object.get("experimental_supported_tools") {
            Some(Value::Array(_)) => {}
            _ => {
                return Err(CatalogError::UnsupportedSchema(
                    slug,
                    "`experimental_supported_tools` must be present and be an array".into(),
                ));
            }
        }

        // `model ... is missing both base_instructions and model_messages`.
        let has_instructions =
            matches!(
                model_object.get("base_instructions"),
                Some(Value::String(_))
            ) || matches!(model_object.get("model_messages"), Some(Value::Object(_)));
        if !has_instructions {
            return Err(CatalogError::UnsupportedSchema(
                slug,
                "one of `base_instructions` or `model_messages` must be present and non-null"
                    .into(),
            ));
        }
    }
    Ok(())
}

pub fn official_model_ids(catalog: &Value) -> Result<Vec<String>, CatalogError> {
    validate_catalog(catalog)?;
    Ok(catalog["models"]
        .as_array()
        .expect("validated catalog has models")
        .iter()
        .filter_map(|model| model.get("slug").and_then(Value::as_str).map(str::to_owned))
        .collect())
}

/// Return a stable fingerprint of the catalog's structural schema rather than
/// its volatile model values.  Integration stores this alongside the
/// generated catalog so a future Codex binary cannot silently consume an
/// unreviewed shape.
pub fn schema_fingerprint(catalog: &Value) -> Result<String, CatalogError> {
    validate_catalog(catalog)?;
    let object = catalog.as_object().ok_or(CatalogError::InvalidShape)?;
    let top_level = object
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let model = object
        .get("models")
        .and_then(Value::as_array)
        .and_then(|models| models.first())
        .and_then(Value::as_object)
        .map(|model| {
            model
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>()
        })
        .ok_or(CatalogError::InvalidShape)?;
    let shape = json!({
        "top_level": top_level,
        "model": model,
        "required": ["slug", "shell_type", "model_messages"],
    });
    let bytes = serde_json::to_vec(&shape)?;
    let mut digest = Sha256::new();
    digest.update(bytes);
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
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

/// The `shell_type` a custom entry advertises.
///
/// Must be a string (Codex rejects `null` here); this is the value every official
/// entry uses.
const DEFAULT_CUSTOM_SHELL_TYPE: &str = "unified_exec";

/// Template fields that describe an official model's exclusive capabilities and
/// are safe to neutralise to `null`.
///
/// A custom entry is built by cloning an official model entry so that required
/// schema fields survive a Codex upgrade. These fields must not keep the official
/// value: they assert official-only instructions, and the Router cannot honour
/// them for a third-party provider. `null` is accepted by Codex for each of them
/// (verified against the real app-server).
const NULLABLE_OFFICIAL_ONLY_TEMPLATE_FIELDS: &[&str] = &[
    "usage_instructions",
    "supports_web_search",
    "apply_patch_tool_type",
    "tool_mode",
];

/// Instructions a custom entry carries so Codex accepts it.
///
/// Codex requires **at least one** of `base_instructions` / `model_messages` on
/// every catalog entry; nulling both produced
/// `model ... is missing both base_instructions and model_messages` and Codex
/// rejected the whole catalog. Verified against the real app-server: nulling
/// either one alone is accepted, so the pair is the constraint.
///
/// A custom model is served by a third-party provider, so the official
/// `base_instructions` (which describe Codex's own agent) must not be reused
/// verbatim. These are deliberately minimal and provider-neutral.
const CUSTOM_BASE_INSTRUCTIONS: &str = "You are a helpful assistant.";

/// Minimal `model_messages` for a custom entry.
///
/// `persistent_instructions` and `instructions_template` are the two keys the
/// official entries always carry; both are required by the schema.
const CUSTOM_MODEL_MESSAGES_JSON: &str = r#"{
    "persistent_instructions": "You are a helpful assistant.",
    "instructions_template": "{{ instructions }}"
}"#;

/// Template fields that are official-only **but cannot be `null`**.
///
/// Codex's catalog schema rejects `null` for these with
/// `invalid type: null, expected string or map` (and `expected a boolean` /
/// `expected a sequence`), and a single invalid entry makes Codex reject the
/// **entire catalog** and silently fall back to its built-in models — so every
/// custom model vanished. Verified against the real app-server:
///
/// | field | required type | value used here |
/// |---|---|---|
/// | `shell_type` | string or map | `"unified_exec"` (what every official entry uses) |
/// | `supports_search_tool` | bool | `false` (the Router does not proxy search) |
/// | `experimental_supported_tools` | sequence | `[]` |
///
/// They are set to valid, capability-neutral values instead of `null`.
const TYPED_OFFICIAL_ONLY_TEMPLATE_FIELDS: &[(&str, &str)] = &[
    ("supports_search_tool", "false"),
    ("experimental_supported_tools", "[]"),
];

fn custom_entry(template: &Value, provider_name: &str, model: &CustomModel) -> Value {
    let mut object = template
        .as_object()
        .expect("validated Codex model entries are objects")
        .clone();

    for official_only in NULLABLE_OFFICIAL_ONLY_TEMPLATE_FIELDS {
        object.insert((*official_only).into(), Value::Null);
    }
    // `shell_type` must stay a string; every official entry uses `unified_exec`.
    set_string(&mut object, "shell_type", DEFAULT_CUSTOM_SHELL_TYPE);
    // At least one of these two must be present *and* non-null.
    set_string(&mut object, "base_instructions", CUSTOM_BASE_INSTRUCTIONS);
    object.insert(
        "model_messages".into(),
        serde_json::from_str(CUSTOM_MODEL_MESSAGES_JSON).unwrap_or(Value::Null),
    );
    for (field, raw) in TYPED_OFFICIAL_ONLY_TEMPLATE_FIELDS {
        object.insert(
            (*field).into(),
            serde_json::from_str(raw).unwrap_or(Value::Null),
        );
    }

    set_string(&mut object, "slug", &model.logical_model_id);
    let display_name = if model.display_name.is_empty() {
        format!("{provider_name} / {}", model.upstream_model_id)
    } else {
        model.display_name.clone()
    };
    set_string(&mut object, "display_name", &display_name);
    set_string(
        &mut object,
        "description",
        &format!("Custom model routed by Codex MultiProvider to {provider_name}."),
    );
    object.insert("visibility".into(), Value::String("list".into()));
    object.insert("supported_in_api".into(), Value::Bool(true));
    object.insert("priority".into(), json!(1000));
    object.insert("additional_speed_tiers".into(), Value::Array(Vec::new()));
    object.insert("service_tiers".into(), Value::Array(Vec::new()));
    object.insert("default_service_tier".into(), Value::Null);
    object.insert("upgrade".into(), Value::Null);
    object.insert("availability_nux".into(), Value::Null);
    object.insert("include_apps_usage_instructions".into(), Value::Bool(false));
    object.insert(
        "include_plugin_usage_instructions".into(),
        Value::Bool(false),
    );
    object.insert(
        "include_skills_usage_instructions".into(),
        Value::Bool(false),
    );
    object.insert(
        "supports_reasoning_summary_parameter".into(),
        Value::Bool(model.capabilities.reasoning),
    );
    object.insert(
        "default_reasoning_summary".into(),
        Value::String(
            if model.capabilities.reasoning {
                "auto"
            } else {
                "none"
            }
            .into(),
        ),
    );
    object.insert("support_verbosity".into(), Value::Bool(false));
    object.insert("default_verbosity".into(), Value::Null);
    object.insert("apply_patch_tool_type".into(), Value::Null);
    object.insert("web_search_tool_type".into(), Value::String("text".into()));
    object.insert(
        "truncation_policy".into(),
        json!({"mode": "tokens", "limit": 10_000}),
    );
    object.insert("supports_image_detail_original".into(), Value::Bool(false));
    object.insert(
        "experimental_supported_tools".into(),
        Value::Array(Vec::new()),
    );
    object.insert("supports_search_tool".into(), Value::Bool(false));
    object.insert("supports_experimental_context".into(), Value::Bool(false));
    object.insert("use_responses_lite".into(), Value::Bool(false));
    object.insert("node_repl_auto_review_required".into(), Value::Bool(false));
    object.insert("node_repl_disabled".into(), Value::Bool(true));
    object.insert("tool_mode".into(), Value::Null);
    object.insert("multi_agent_version".into(), Value::Null);
    object.insert("multi_agent_reasoning_effort".into(), Value::Null);
    // Function tools are not advertised until the Router can prove the full
    // tool lifecycle and context boundary for the selected provider.
    object.insert(
        "supports_parallel_tool_calls".into(),
        Value::Bool(model.capabilities.tools),
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
    // `supported_reasoning_levels` is a **required** field in Codex's catalog
    // schema. Removing it for a model without reasoning levels made Codex reject
    // the *entire* catalog with
    // `missing field supported_reasoning_levels` and silently fall back to its
    // built-in defaults — so every custom model disappeared.
    //
    // A registry written by an older release (or hand-edited) deserializes
    // `reasoning_levels` to an empty vec, so this is reachable. Emit an empty
    // array instead of dropping the key: valid schema, and it truthfully says
    // "this model exposes no selectable reasoning levels".
    let levels: Vec<Value> = model
        .reasoning_levels
        .iter()
        .map(|effort| json!({ "effort": effort, "description": format!("{effort} reasoning") }))
        .collect();
    object.insert("supported_reasoning_levels".into(), Value::Array(levels));
    match model.reasoning_levels.first() {
        Some(default) => {
            object.insert(
                "default_reasoning_level".into(),
                Value::String(default.clone()),
            );
        }
        // No levels to default to; drop the *optional* default rather than
        // pointing at a level that does not exist.
        None => {
            object.remove("default_reasoning_level");
        }
    }
    object.insert(
        "supports_reasoning_summaries".into(),
        Value::Bool(model.capabilities.reasoning),
    );
    Value::Object(object)
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
    let mut bytes = serde_json::to_vec_pretty(catalog)?;
    bytes.push(b'\n');
    // Created 0600: the catalog is sized and shaped by the user's provider list,
    // and the surrounding directory also holds the credential file.
    codex_mp_core::write_private_atomic(path, &bytes)?;
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
                "shell_type": "unified_exec",
                "model_messages": {"persistent_instructions": "safe", "instructions_template": "safe"},
                // These three are required-with-a-type by Codex's schema and are
                // present on every entry of the real official catalog. The stub
                // must include them or `validate_catalog` correctly rejects it.
                "supports_search_tool": true,
                "experimental_supported_tools": [],
                "base_instructions": "official instructions",
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
        assert_eq!(merged["models"][1]["unknown_future_field"]["keep"], true);
        // Official-only capabilities must be neutralised, not inherited — but
        // they must stay **schema-valid**. Codex rejects `null` for `shell_type`,
        // `supports_search_tool` and `experimental_supported_tools`, and rejects
        // an entry that has neither `base_instructions` nor `model_messages`;
        // any of those makes it discard the *entire* catalog and silently fall
        // back to its built-in models, hiding every custom model.
        assert_eq!(merged["models"][1]["shell_type"], DEFAULT_CUSTOM_SHELL_TYPE);
        assert_eq!(merged["models"][1]["supports_search_tool"], false);
        assert_eq!(merged["models"][1]["supports_parallel_tool_calls"], false);
        assert_eq!(
            merged["models"][1]["experimental_supported_tools"],
            json!([])
        );
        // The official instructions must not be reused, but the fields have to
        // carry provider-neutral values.
        assert_ne!(
            merged["models"][1]["base_instructions"], merged["models"][0]["base_instructions"],
            "a custom entry must not reuse the official base_instructions"
        );
        assert!(
            merged["models"][1]["base_instructions"].is_string(),
            "base_instructions must be a string, not null"
        );
        assert!(
            merged["models"][1]["model_messages"].is_object(),
            "model_messages must be a map, not null"
        );
    }

    #[test]
    fn custom_entry_advertises_only_declared_capabilities() {
        let dir = tempdir().unwrap();
        let mut registry = ProviderRegistry::empty(dir.path().join("providers.json"));
        registry
            .add_provider(ProviderConfig::new("NewAPI", "https://example.test/v1").unwrap())
            .unwrap();
        let mut model = CustomModel::new("newapi", "qwen3.8", "NewAPI / Qwen3.8").unwrap();
        model.capabilities.tools = true;
        model.capabilities.images = true;
        registry.add_model(model).unwrap();

        let merged = merge_catalog(&official(), &registry).unwrap();
        let custom = &merged["models"][1];
        assert_eq!(custom["supports_parallel_tool_calls"], true);
        assert_eq!(custom["input_modalities"], json!(["text", "image"]));
        // Declared capabilities do not resurrect official-only fields. Codex
        // requires concrete values here (see `merge_keeps_official_and_unknown_fields`).
        assert_eq!(custom["shell_type"], DEFAULT_CUSTOM_SHELL_TYPE);
        assert_eq!(custom["supports_search_tool"], false);
        assert!(custom["model_messages"].is_object());
        assert!(custom["base_instructions"].is_string());
    }

    /// Regression: a custom entry was rejected by stock Codex because three
    /// official-only fields were neutralised to `null`, and because *both*
    /// `base_instructions` and `model_messages` were nulled.
    ///
    /// Codex rejects the **entire** catalog on the first invalid entry and
    /// silently falls back to its built-in models, so every custom model became
    /// invisible. The shapes below were confirmed against the real app-server:
    ///
    /// * `shell_type`   — `invalid type: null, expected string or map`
    /// * `supports_search_tool` — `invalid type: null, expected a boolean`
    /// * `experimental_supported_tools` — `invalid type: null, expected a sequence`
    /// * `base_instructions` + `model_messages` — `is missing both ...`
    ///
    /// This test pins the *types*, which is what the schema actually checks.
    #[test]
    fn custom_entries_are_schema_valid_for_stock_codex() {
        let dir = tempdir().unwrap();
        let mut registry = ProviderRegistry::empty(dir.path().join("providers.json"));
        registry
            .add_provider(ProviderConfig::new("NewAPI", "https://example.test/v1").unwrap())
            .unwrap();
        registry
            .add_model(CustomModel::new("newapi", "qwen3.8", "NewAPI / Qwen3.8").unwrap())
            .unwrap();

        let merged = merge_catalog(&official(), &registry).unwrap();
        let custom = &merged["models"][1];

        // Must be present and non-null, with the right type.
        assert!(
            custom["shell_type"].is_string(),
            "shell_type must be a string, got {}",
            custom["shell_type"]
        );
        assert!(
            custom["supports_search_tool"].is_boolean(),
            "supports_search_tool must be a boolean, got {}",
            custom["supports_search_tool"]
        );
        assert!(
            custom["experimental_supported_tools"].is_array(),
            "experimental_supported_tools must be an array, got {}",
            custom["experimental_supported_tools"]
        );
        assert!(
            custom["base_instructions"].is_string() || custom["model_messages"].is_object(),
            "Codex requires base_instructions or model_messages to be present and non-null"
        );
        assert!(
            custom["supported_reasoning_levels"].is_array(),
            "supported_reasoning_levels is required, got {}",
            custom["supported_reasoning_levels"]
        );

        // Every entry in the merged catalog must satisfy this, not just the one
        // we happened to inspect: one bad entry invalidates the whole file.
        for entry in merged["models"].as_array().unwrap() {
            let slug = entry["slug"].as_str().unwrap_or("<unknown>");
            assert!(
                entry["shell_type"].is_string(),
                "`{slug}` has a non-string shell_type: {}",
                entry["shell_type"]
            );
            assert!(
                entry["supported_reasoning_levels"].is_array(),
                "`{slug}` is missing supported_reasoning_levels"
            );
            assert!(
                entry["base_instructions"].is_string() || entry["model_messages"].is_object(),
                "`{slug}` has neither base_instructions nor model_messages"
            );
        }
    }

    /// A model whose `reasoning_levels` is empty (an older registry, or a
    /// hand-edited one, deserializes to an empty vec) must still produce a valid
    /// entry: the fix is to emit an empty array, never to drop the required key.
    #[test]
    fn a_model_without_reasoning_levels_still_emits_the_required_field() {
        let dir = tempdir().unwrap();
        let mut registry = ProviderRegistry::empty(dir.path().join("providers.json"));
        registry
            .add_provider(ProviderConfig::new("NewAPI", "https://example.test/v1").unwrap())
            .unwrap();
        let mut model = CustomModel::new("newapi", "qwen3.8", "NewAPI / Qwen3.8").unwrap();
        model.reasoning_levels = Vec::new();
        registry.add_model(model).unwrap();

        let merged = merge_catalog(&official(), &registry).unwrap();
        let custom = &merged["models"][1];
        assert!(
            custom["supported_reasoning_levels"].is_array(),
            "the required key must survive an empty reasoning_levels"
        );
        assert_eq!(custom["supported_reasoning_levels"], json!([]));
        // The optional default must not point at a level that does not exist.
        assert!(
            custom.get("default_reasoning_level").is_none()
                || custom["default_reasoning_level"].is_null(),
            "default_reasoning_level must not name a non-existent level"
        );
    }

    /// Regression: `validate_catalog` checked only that a *key* was present, so
    /// `{"shell_type": null}` passed validation and reached Codex, which rejects
    /// the whole catalog on the first invalid entry. Each case below is a shape
    /// that the real app-server rejected (message quoted in the assertion).
    #[test]
    fn validate_catalog_rejects_the_shapes_stock_codex_rejects() {
        let base = official();
        let entry = |mutate: &dyn Fn(&mut Value)| {
            let mut catalog = base.clone();
            mutate(&mut catalog["models"][0]);
            catalog
        };

        // Sanity: the unmodified stub is valid (guards against a vacuous test).
        assert!(validate_catalog(&base).is_ok());

        let null_shell = entry(&|m| m["shell_type"] = Value::Null);
        assert!(
            validate_catalog(&null_shell).is_err(),
            "`invalid type: null, expected string or map` — must be rejected"
        );

        let null_search = entry(&|m| m["supports_search_tool"] = Value::Null);
        assert!(
            validate_catalog(&null_search).is_err(),
            "`invalid type: null, expected a boolean` — must be rejected"
        );

        let null_tools = entry(&|m| m["experimental_supported_tools"] = Value::Null);
        assert!(
            validate_catalog(&null_tools).is_err(),
            "`invalid type: null, expected a sequence` — must be rejected"
        );

        let no_levels = entry(&|m| {
            m.as_object_mut()
                .unwrap()
                .remove("supported_reasoning_levels");
        });
        assert!(
            validate_catalog(&no_levels).is_err(),
            "`missing field supported_reasoning_levels` — must be rejected"
        );

        let no_instructions = entry(&|m| {
            m["base_instructions"] = Value::Null;
            m["model_messages"] = Value::Null;
        });
        assert!(
            validate_catalog(&no_instructions).is_err(),
            "`is missing both base_instructions and model_messages` — must be rejected"
        );

        // Either instruction field alone is enough, matching the real schema.
        let only_model_messages = entry(&|m| m["base_instructions"] = Value::Null);
        assert!(validate_catalog(&only_model_messages).is_ok());
        let only_base = entry(&|m| m["model_messages"] = Value::Null);
        assert!(validate_catalog(&only_base).is_ok());
    }

    /// Regression: `--bundled` is the authoritative form, but a Codex build that
    /// predates the flag rejects it. Failing outright made `sync` unusable on such
    /// a version, so discovery must fall back to the plain command.
    #[cfg(unix)]
    #[test]
    fn discovery_falls_back_when_bundled_is_not_supported() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let stub = dir.path().join("codex-old");
        // Rejects `--bundled` exactly as an older CLI does, and answers the plain
        // form with a minimal valid catalog.
        fs::write(
            &stub,
            "#!/bin/sh\n\
             for a in \"$@\"; do\n\
             \x20 if [ \"$a\" = \"--bundled\" ]; then\n\
             \x20   echo \"error: unexpected argument '--bundled' found\" >&2\n\
             \x20   exit 2\n\
             \x20 fi\n\
             done\n\
             printf '%s\\n' '{\"models\":[{\"slug\":\"gpt-5.5\",\"display_name\":\"GPT-5.5\",\"shell_type\":\"unified_exec\",\"model_messages\":{\"persistent_instructions\":\"s\",\"instructions_template\":\"s\"},\"supported_reasoning_levels\":[],\"supports_search_tool\":false,\"experimental_supported_tools\":[]}]}'\n",
        )
        .unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();

        let catalog = discover_official_catalog(&stub)
            .expect("discovery must fall back when --bundled is rejected");
        validate_catalog(&catalog).unwrap();
        assert_eq!(catalog["models"][0]["slug"], "gpt-5.5");
    }

    /// A binary that fails both forms must still report an error (not panic, not
    /// silently return an empty catalog).
    #[cfg(unix)]
    #[test]
    fn discovery_reports_an_error_when_neither_form_works() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let stub = dir.path().join("codex-broken");
        fs::write(&stub, "#!/bin/sh\necho 'boom' >&2\nexit 3\n").unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(discover_official_catalog(&stub).is_err());
    }

    /// Regression: `schema_fingerprint`'s doc says it exists so "a future Codex
    /// binary cannot silently consume an unreviewed shape", but nothing ever read
    /// the stored value — an upstream schema change was applied silently.
    ///
    /// This pins the *property the fingerprint must have* for that check to mean
    /// anything: it must be stable for a given shape and change when the shape
    /// changes.
    #[test]
    fn the_schema_fingerprint_is_stable_and_shape_sensitive() {
        let base = official();
        let first = schema_fingerprint(&base).unwrap();
        let again = schema_fingerprint(&base).unwrap();
        assert_eq!(first, again, "the fingerprint must be stable");

        // Changing a model *value* must not change the structural fingerprint...
        let mut value_changed = base.clone();
        value_changed["models"][0]["display_name"] = json!("A different name");
        assert_eq!(
            schema_fingerprint(&value_changed).unwrap(),
            first,
            "volatile model values must not affect the structural fingerprint"
        );

        // ...but adding a field to the shape must.
        let mut shape_changed = base.clone();
        shape_changed["models"][0]["brand_new_field_2026"] = json!(true);
        assert_ne!(
            schema_fingerprint(&shape_changed).unwrap(),
            first,
            "a changed shape must produce a different fingerprint"
        );
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

    #[test]
    fn schema_fingerprint_is_structural_and_stable() {
        let first = schema_fingerprint(&official()).unwrap();
        let mut changed = official();
        changed["models"][0]["display_name"] = json!("different value");
        assert_eq!(first, schema_fingerprint(&changed).unwrap());
        changed["models"][0]["future_structural_field"] = json!(true);
        assert_ne!(first, schema_fingerprint(&changed).unwrap());
    }
}
