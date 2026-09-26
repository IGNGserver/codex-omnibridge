//! Codex model catalog discovery and schema-preserving merge.
//!
//! The official catalog is sourced from the installed Codex binary rather than
//! a GitHub `main` snapshot. Unknown fields are retained verbatim by merging
//! JSON values instead of deserializing into a local approximation of Codex's
//! fast-moving schema.

use codex_mp_core::{CustomModel, ProviderRegistry, command_for_executable};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Deadline for reading the bundled catalog out of the Codex binary.
const CATALOG_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CATALOG_MODELS: usize = 8192;
const MAX_CATALOG_SLUG_CHARS: usize = 1024;
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
    if models.len() > MAX_CATALOG_MODELS {
        return Err(CatalogError::UnsupportedSchema(
            "<catalog>".into(),
            format!("models array exceeds the {MAX_CATALOG_MODELS} item limit"),
        ));
    }
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
        if slug != "<unknown>" && slug.chars().count() > MAX_CATALOG_SLUG_CHARS {
            return Err(CatalogError::UnsupportedSchema(
                "<catalog>".into(),
                format!("model slug exceeds the {MAX_CATALOG_SLUG_CHARS} character limit"),
            ));
        }

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
    // Cloned into every custom entry, so the choice decides which official
    // model's harness settings a third-party model inherits. See
    // `select_template` for why "the first one" is the wrong answer.
    let template = select_template(models)
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

/// The `shell_type` used only when the template carries none.
///
/// Must be a string (Codex rejects `null` here); `unified_exec` is the value
/// every official entry of every catalog observed so far uses, so in practice
/// the template's own value is inherited and this is only a safety net.
const FALLBACK_CUSTOM_SHELL_TYPE: &str = "unified_exec";

/// Instructions used only when the template carries none.
///
/// `validate_catalog` already requires every entry to have `base_instructions`
/// or `model_messages`, so this is unreachable for a catalog Codex itself
/// produced. It exists so a hand-written fixture cannot yield an entry with no
/// prompt at all.
const FALLBACK_CUSTOM_INSTRUCTIONS: &str = "You are Codex, an interactive coding agent. \
You help the user with software engineering tasks in their workspace.";

/// Catalog fields describing **OpenAI-hosted services** that a third-party
/// provider genuinely cannot fulfil.
///
/// These must not keep the official value: `supports_web_search` /
/// `supports_search_tool` drive the hosted `web_search` tool, which only the
/// OpenAI backend can execute. The Router cannot synthesise it, so advertising it
/// would make Codex emit a tool the upstream rejects.
///
/// Fields listed here are nulled **only if the installed Codex declares them**.
/// Neither `supports_web_search` nor `usage_instructions` exists in the 0.156.0
/// catalog; an earlier revision inserted them unconditionally, which put keys in
/// the generated catalog that Codex itself would never produce.
const HOSTED_SERVICE_TEMPLATE_FIELDS: &[&str] = &["supports_web_search", "usage_instructions"];

/// Catalog fields describing Codex's **client-side harness conventions**.
///
/// These are inherited from the official template rather than neutralised. An
/// earlier revision nulled them on the theory that they "assert official-only
/// instructions the Router cannot honour". That theory is wrong for these
/// fields, and the cost was severe — measured against stock Codex 0.156.0:
///
/// | field | official value | nulled to | effect on a custom model |
/// |---|---|---|---|
/// | `apply_patch_tool_type` | `"freeform"` | `null` | **no apply_patch tool at all** |
/// | `multi_agent_version` | `"v2"` | `null` | legacy `multi_agent_v1__*` tools |
/// | `experimental_supported_tools` | `["send_user_message_async","clock"]` | `[]` | lost `sleep` / async input |
///
/// None of these are OpenAI-hosted services. They are conventions Codex
/// implements locally and documents to the model through its instructions, so a
/// third-party model needs them exactly as much as an official one does. What it
/// cannot honour is hosted search and image generation, which stay disabled via
/// `HOSTED_SERVICE_TEMPLATE_FIELDS` and `supports_search_tool`.
///
/// Inheritance is conditional on `ModelCapabilities::tools`: that flag is what
/// makes the panel's "supports tools" switch mean something. When it is off, the
/// harness fields are neutralised so Codex advertises no tools at all.
///
/// `tool_mode` is deliberately **not** here — see
/// [`CODE_MODE_TOOL_FIELDS`].
const HARNESS_CONVENTION_TEMPLATE_FIELDS: &[&str] = &[
    "apply_patch_tool_type",
    "multi_agent_version",
    "multi_agent_reasoning_effort",
    "experimental_supported_tools",
];

/// Catalog fields that switch Codex to its **code-mode tool surface**.
///
/// Codex 0.156.0 ships `tool_mode: "code_mode_only"` on its official entries.
/// An earlier revision of this crate inherited that value alongside
/// `apply_patch_tool_type`; a six-variant matrix of real `codex exec` turns
/// against stock 0.156.0 shows why that is wrong. The tools Codex advertises,
/// measured on the wire:
///
/// | variant (`tool_mode` / `apply_patch_tool_type`) | advertised tool surface |
/// |---|---|
/// | `"code_mode_only"` / `"freeform"` | `custom:exec` (format `grammar`), `wait`, `request_user_input` — **no `exec_command`, no `apply_patch`** |
/// | `"code_mode_only"` / `null` | identical to the row above |
/// | `null` / `"freeform"` | `exec_command`, `write_stdin`, `list_mcp_resources`, `list_mcp_resource_templates`, `read_mcp_resource`, `request_user_input`, `custom:apply_patch` (format `grammar`), `view_image`, `get_goal`, `create_goal`, `update_goal` |
/// | `null` / `null` | the row above minus `custom:apply_patch` |
/// | key removed / either | identical to the matching `null` row |
///
/// So the two fields are independent: `apply_patch_tool_type` alone restores
/// `apply_patch`, and `tool_mode: "code_mode_only"` alone collapses the whole
/// flat tool set into a single `exec` tool. That `exec` tool takes **raw
/// JavaScript for a fresh V8 isolate** — its own schema says "no Node, no file
/// system, no network access, no console" and "Accepts raw JavaScript source
/// text, not JSON" — and nested tools are only reachable as
/// `await tools.exec_command(...)`. The GPT models Codex ships are trained on
/// that surface; NewAPI / OneAPI / OpenRouter / local models are not, so
/// inheriting it would hand a third-party model a tool it cannot drive while
/// hiding the eleven flat tools it can.
///
/// `null` and key-absent are measurably equivalent, so these are neutralised in
/// place (never invented) by the shared `!tools` path and by the unconditional
/// pass in [`custom_entry`].
const CODE_MODE_TOOL_FIELDS: &[&str] = &["tool_mode"];

/// Fields whose official value depends on an OpenAI-backend protocol variant.
///
/// `use_responses_lite` and `supports_experimental_context` change what Codex
/// puts on the wire. They are inherited only when the model declares tool
/// support, because the code-mode tool layout they accompany is what makes them
/// meaningful; a text-only model gets the neutral value instead.
const BACKEND_PROTOCOL_TEMPLATE_FIELDS: &[&str] =
    &["use_responses_lite", "supports_experimental_context"];

/// Rewrite the model-identity sentence of an official prompt so it does not tell
/// a third-party model that it is a specific GPT.
///
/// Every official `instructions_template` opens with one of:
///
/// ```text
/// You are Codex, an agent based on GPT-6. You and the user share one workspace…
/// You are Codex, a coding agent based on GPT-5. You and the user…
/// ```
///
/// Only that first sentence is replaced; the remaining ~21k characters describe
/// the harness (apply_patch format, shell conventions, approval policy) and are
/// exactly what a custom model needs in order to function as a Codex agent.
///
/// The match is deliberately narrow. Anything that does not start with
/// `You are Codex, ` *and* contain ` based on ` is returned untouched, so an
/// upstream rewording degrades to "keep the official text" rather than to a
/// mangled prompt. The sentence boundary is `". "` — not `"."` — because model
/// names contain dots (`GPT-5.6-Sol`).
fn neutralize_model_identity(instructions: &str) -> String {
    const PREFIX: &str = "You are Codex, ";
    const NEUTRAL: &str = "You are Codex, an interactive coding agent.";
    if !instructions.starts_with(PREFIX) || !instructions.contains(" based on ") {
        return instructions.to_owned();
    }
    match instructions.find(". ") {
        Some(index) => format!("{NEUTRAL}{}", &instructions[index + 1..]),
        None => instructions.to_owned(),
    }
}

/// The agent instructions a custom entry carries.
///
/// Sourced from the official template at sync time rather than vendored, so it
/// always matches the Codex version actually installed: a catalog is generated
/// from `codex debug models --bundled` of that very binary.
///
/// `model_messages.instructions_template` is authoritative. Stock Codex 0.156.0
/// was observed to prefer it over the top-level `base_instructions` field — an
/// entry carrying `base_instructions: "You are a helpful assistant."` alongside
/// `instructions_template: "{{ instructions }}"` reached the upstream with the
/// literal 18-byte string `{{ instructions }}` as its system prompt. `{{ … }}`
/// is not a placeholder Codex renders; no official entry uses that syntax. Both
/// fields are therefore set to the same real text.
///
/// A registry `override` wins outright and is used **verbatim**: no identity
/// rewriting, no template fallback. Whoever wrote it owns its contents, and
/// silently editing a user-authored prompt would be worse than not offering the
/// override at all. A blank override degrades to the template path, because
/// `normalize_system_prompt` already treats blank as "no override" and a
/// hand-edited registry should behave the same way rather than send an empty
/// system prompt.
fn custom_instructions(template: &Value, override_prompt: Option<&str>) -> String {
    if let Some(prompt) = override_prompt
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
    {
        return prompt.to_owned();
    }
    let object = match template.as_object() {
        Some(object) => object,
        None => return FALLBACK_CUSTOM_INSTRUCTIONS.to_owned(),
    };
    let from_messages = object
        .get("model_messages")
        .and_then(Value::as_object)
        .and_then(|messages| messages.get("instructions_template"))
        .and_then(Value::as_str);
    let from_base = object.get("base_instructions").and_then(Value::as_str);
    match from_messages.or(from_base) {
        Some(instructions) if !instructions.trim().is_empty() => {
            neutralize_model_identity(instructions)
        }
        _ => FALLBACK_CUSTOM_INSTRUCTIONS.to_owned(),
    }
}

/// Clone the template's whole `model_messages` block, substituting neutral
/// instructions.
///
/// Cloning the block (rather than writing a two-key stub) is what carries the
/// approval policy, collaboration modes, auto-review rules and token-budget
/// reminders over to the custom model. Those are harness documentation, not
/// account privileges, and without them the model cannot drive the tool loop it
/// is being offered.
fn custom_model_messages(template: &Value, instructions: &str) -> Value {
    let mut messages = template
        .get("model_messages")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_else(Map::new);
    messages.insert(
        "instructions_template".into(),
        Value::String(instructions.to_owned()),
    );
    Value::Object(messages)
}

/// Replace a field's value with the neutral value **of the same type**.
///
/// Codex rejects `null` where its schema demands a concrete type, and a single
/// invalid entry makes it discard the *entire* catalog and silently fall back to
/// its built-in models. The neutral value is derived from the template's own type
/// instead of being hardcoded per field, so a future Codex that changes a field's
/// representation still receives something it can parse:
///
/// | template type | neutral value |
/// |---|---|
/// | array | `[]` |
/// | bool | `false` |
/// | object | `{}` |
/// | number | `0` |
/// | string / null | `null` (the schema for these is `string \| null`) |
///
/// A key the template does not have is **not** created — see
/// [`override_if_present`].
fn neutralize_field(object: &mut Map<String, Value>, key: &str) {
    let neutral = match object.get(key) {
        Some(Value::Array(_)) => Value::Array(Vec::new()),
        Some(Value::Bool(_)) => Value::Bool(false),
        Some(Value::Object(_)) => Value::Object(Map::new()),
        Some(Value::Number(_)) => json!(0),
        Some(_) => Value::Null,
        None => return,
    };
    object.insert(key.to_owned(), neutral);
}

/// Override a template field only when the installed Codex actually declares it.
///
/// Inserting a key the current schema does not have creates a *phantom* field.
/// It is inert today, but it pins behaviour the moment Codex introduces a real
/// field of the same name, and it makes the generated catalog differ from
/// anything Codex itself would produce — which defeats the point of cloning an
/// official entry. Four such fields (`supports_parallel_tool_calls`,
/// `supports_reasoning_summaries`, `supports_reasoning_summary_parameter`,
/// `default_service_tier`) were emitted unconditionally by an earlier revision
/// and exist in no entry of the 0.156.0 catalog.
fn override_if_present(object: &mut Map<String, Value>, key: &str, value: Value) {
    if object.contains_key(key) {
        object.insert(key.to_owned(), value);
    }
}

/// Identity/marketing fields that describe an *official* model and have no
/// meaning for a third-party one. Removed rather than overridden so the entry
/// does not claim a specialty it does not have.
const OFFICIAL_IDENTITY_FIELDS: &[&str] = &["model_specialty"];

/// Pick the official entry that best represents the catalog as a whole.
///
/// [`custom_entry`] clones one official entry so every schema-required field
/// survives a Codex upgrade. Taking the *first* entry — what an earlier revision
/// did — is wrong: in the 0.156.0 catalog that is `gpt-6-astra`, which carries
/// settings no other model has (`node_repl_auto_review_required: true`,
/// `supports_experimental_context: true`, a non-empty
/// `experimental_supported_tools`, `multi_agent_reasoning_effort: "xhigh"`, a
/// 21k-character prompt). Cloning it hands every third-party model astra's
/// exclusives.
///
/// Each entry is instead scored by the fraction of the catalog that shares each
/// of its field values, and the highest scorer wins (ties → catalog order, so the
/// result is deterministic). Identity fields are excluded from scoring because
/// they are unique per model by construction and are always overridden anyway.
/// On the 0.156.0 catalog this selects `gpt-5.6-terra` (score 28.7) over
/// `gpt-6-astra` (24.7) — the modal profile.
fn select_template(models: &[Value]) -> Option<&Value> {
    /// Scoring is quadratic in the number of entries, and a catalog may hold up
    /// to `MAX_CATALOG_MODELS`. Only the head of the list is scored: the choice
    /// needs to be *representative*, not exhaustive, and every real Codex
    /// catalog observed so far has fewer than twenty entries.
    const MAX_TEMPLATE_CANDIDATES: usize = 64;
    const UNSCORED: [&str; 4] = ["slug", "display_name", "description", "priority"];

    let candidates: Vec<&Value> = models
        .iter()
        .filter(|model| model.is_object())
        .take(MAX_TEMPLATE_CANDIDATES)
        .collect();
    if candidates.is_empty() {
        return None;
    }
    // Field values are compared by a digest of their serialized form: entries
    // mix strings, booleans, arrays and deeply nested `model_messages` blocks, so
    // "same JSON" is the only meaningful equality. Digesting keeps the score table
    // small even though a prompt field is ~21 KB.
    let encoded: Vec<Vec<(String, u64)>> = candidates
        .iter()
        .map(|entry| {
            let mut pairs: Vec<(String, u64)> = entry
                .as_object()
                .expect("filtered to objects above")
                .iter()
                .filter(|(key, _)| !UNSCORED.contains(&key.as_str()))
                .map(|(key, value)| (key.clone(), value_digest(value)))
                .collect();
            pairs.sort();
            pairs
        })
        .collect();
    let mut shared: HashMap<(&str, u64), usize> = HashMap::new();
    for entry in &encoded {
        for (key, digest) in entry {
            *shared.entry((key.as_str(), *digest)).or_default() += 1;
        }
    }
    let total = encoded.len() as f64;
    let mut best_index = 0;
    let mut best_score = f64::NEG_INFINITY;
    for (index, entry) in encoded.iter().enumerate() {
        let score: f64 = entry
            .iter()
            .map(|(key, digest)| {
                let count = shared.get(&(key.as_str(), *digest)).copied().unwrap_or(0);
                f64::from(count as u32) / total
            })
            .sum();
        if score > best_score {
            best_score = score;
            best_index = index;
        }
    }
    candidates.get(best_index).copied()
}

fn value_digest(value: &Value) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.to_string().hash(&mut hasher);
    hasher.finish()
}

/// Build one custom catalog entry by cloning an official one.
///
/// The governing rule is **inherit unless there is a reason not to**. An entry
/// that differs from what Codex itself generated is an entry that can break on
/// the next upgrade, so every override below names the reason it exists:
///
/// 1. *identity* — slug, display name, description, discovery metadata;
/// 2. *instructions* — the official prompt, with only its model-identity sentence
///    rewritten (see [`custom_instructions`]), unless the registry supplies its
///    own;
/// 3. *OpenAI-hosted services* the Router cannot execute — off;
/// 4. *account products* (speed tiers, service tiers, upgrades, apps/plugins/
///    skills surfaces, verbosity) — off, because a third-party key has none;
/// 5. *the code-mode tool surface* ([`CODE_MODE_TOOL_FIELDS`]) — off, because it
///    replaces the flat tools a third-party model can actually drive;
/// 6. *registry-declared capabilities* — modalities, context window, reasoning.
///
/// Everything else, including the rest of the harness contract
/// (`apply_patch_tool_type`, `shell_type`, `truncation_policy`,
/// `effective_context_window_percent`, `comp_hash`, the `model_messages` policy
/// blocks), is inherited verbatim.
fn custom_entry(template: &Value, provider_name: &str, model: &CustomModel) -> Value {
    let mut object = template
        .as_object()
        .expect("validated Codex model entries are objects")
        .clone();
    let tools = model.capabilities.tools;
    let reasoning = model.capabilities.reasoning;

    // ── 1. identity ────────────────────────────────────────────────────────────
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
        &format!("Custom model routed by Codex OmniBridge to {provider_name}."),
    );
    object.insert("visibility".into(), Value::String("list".into()));
    object.insert("supported_in_api".into(), Value::Bool(true));
    // Sorted below the official models: the picker should not start on a
    // third-party entry.
    object.insert("priority".into(), json!(1000));
    for field in OFFICIAL_IDENTITY_FIELDS {
        object.remove(*field);
    }

    // ── 2. instructions ───────────────────────────────────────────────────────
    // Codex requires at least one of these present and non-null, and prefers
    // `model_messages.instructions_template` when both exist. Both are set to the
    // same real text so the preference cannot change what the model is told.
    // `base_instructions` alone already satisfies the requirement, so a
    // `model_messages` block is carried over only when the template has one —
    // inventing an empty block would add a key this Codex build does not use.
    //
    // A registry override replaces the text but not the surrounding
    // `model_messages` policy block, and it does not turn off
    // `apply_patch_tool_type`. That combination is deliberate — the override is an
    // escape hatch for models that misbehave under the stock prompt — but it does
    // mean whoever writes one must keep documenting the `apply_patch` freeform
    // format, or the model is offered a tool whose grammar it has never seen. The
    // CLI help for `model edit --system-prompt` says so.
    let instructions = custom_instructions(template, model.system_prompt_override.as_deref());
    set_string(&mut object, "base_instructions", &instructions);
    if template.get("model_messages").is_some_and(Value::is_object) {
        object.insert(
            "model_messages".into(),
            custom_model_messages(template, &instructions),
        );
    }

    // ── 3. required-with-a-type fields ───────────────────────────────────────
    // `shell_type` must be a string or a map. The template's own value is kept
    // whenever it is one of those and non-empty; `unified_exec` is only filled in
    // as a safety net, because that is what every official entry of every catalog
    // observed so far uses.
    let has_shell_type = matches!(
        object.get("shell_type"),
        Some(Value::String(shell)) if !shell.is_empty()
    ) || matches!(object.get("shell_type"), Some(Value::Object(_)));
    if !has_shell_type {
        set_string(&mut object, "shell_type", FALLBACK_CUSTOM_SHELL_TYPE);
    }
    // `supports_search_tool` must be a boolean, and the hosted `web_search` tool
    // it gates is executed by the OpenAI backend — the Router cannot proxy it.
    object.insert("supports_search_tool".into(), Value::Bool(false));
    for hosted in HOSTED_SERVICE_TEMPLATE_FIELDS {
        neutralize_field(&mut object, hosted);
    }
    // `experimental_supported_tools` must be an array even when tools are off.
    if !object
        .get("experimental_supported_tools")
        .is_some_and(Value::is_array)
    {
        object.insert(
            "experimental_supported_tools".into(),
            Value::Array(Vec::new()),
        );
    }

    // ── 4. harness conventions ───────────────────────────────────────────────
    // Inherited when the model declares tool support, so it gets the same
    // apply_patch / multi-agent contract an official model gets. When it does
    // not, they are neutralised in place rather than left advertising tools the
    // upstream cannot receive — that is what makes the panel's "supports tools"
    // switch mean something.
    if !tools {
        for field in HARNESS_CONVENTION_TEMPLATE_FIELDS
            .iter()
            .chain(BACKEND_PROTOCOL_TEMPLATE_FIELDS.iter())
        {
            neutralize_field(&mut object, field);
        }
    }
    // `tool_mode` is neutralised even for a tool-capable model. It is not a
    // harness convention but a switch to the code-mode tool surface, whose only
    // function tool takes raw JavaScript for a V8 isolate — measured evidence in
    // `CODE_MODE_TOOL_FIELDS`.
    for field in CODE_MODE_TOOL_FIELDS {
        neutralize_field(&mut object, field);
    }
    // Auto-review routes a side request to the **official** `codex-auto-review`
    // model. Leaving it on would send this third-party conversation to OpenAI and
    // bill it against the user's ChatGPT plan, without any UI saying so.
    override_if_present(
        &mut object,
        "node_repl_auto_review_required",
        Value::Bool(false),
    );

    // ── 5. account products a third-party key does not have ──────────────────
    for field in [
        "additional_speed_tiers",
        "service_tiers",
        "default_service_tier",
        "upgrade",
        "availability_nux",
        "include_apps_usage_instructions",
        "include_plugin_usage_instructions",
        "include_skills_usage_instructions",
    ] {
        neutralize_field(&mut object, field);
    }
    // `verbosity` goes on the wire as a request parameter, and the protocol
    // bridge does not translate it. Advertising support would have Codex send a
    // field a Chat Completions upstream rejects.
    override_if_present(&mut object, "support_verbosity", Value::Bool(false));
    override_if_present(&mut object, "default_verbosity", Value::Null);
    // `detail: "original"` is an OpenAI image-serving option, not a portable one.
    override_if_present(
        &mut object,
        "supports_image_detail_original",
        Value::Bool(false),
    );
    override_if_present(
        &mut object,
        "supports_parallel_tool_calls",
        Value::Bool(tools),
    );

    // ── 6. registry-declared capabilities ────────────────────────────────────
    // The advertised input contract follows the registry. An earlier revision
    // always claimed text + (optionally) image, which made file/audio/video
    // models impossible to configure truthfully and made Codex hide those inputs
    // in the picker.
    object.insert(
        "input_modalities".into(),
        Value::Array(
            [
                (model.capabilities.text, "text"),
                (model.capabilities.images, "image"),
                (model.capabilities.files, "file"),
                (model.capabilities.audio, "audio"),
                (model.capabilities.video, "video"),
            ]
            .into_iter()
            .filter(|(enabled, _)| *enabled)
            .map(|(_, modality)| Value::String(modality.into()))
            .collect(),
        ),
    );

    // A declared window is authoritative. An *undeclared* one must not fall back
    // to the template's: inheriting `gpt-5.6-terra`'s 272 000 / 872 000 told Codex
    // a 8k model had room for 272k tokens, so it packed the turn and the upstream
    // rejected it. Removing both keys makes Codex apply its own default.
    match model.context_window {
        Some(context_window) => {
            object.insert("context_window".into(), json!(context_window));
            object.insert("max_context_window".into(), json!(context_window));
        }
        None => {
            object.remove("context_window");
            object.remove("max_context_window");
        }
    }

    // `supported_reasoning_levels` is **required**. Dropping it for a model
    // without levels made Codex reject the *entire* catalog with
    // `missing field supported_reasoning_levels` and silently fall back to its
    // built-in defaults — so every custom model disappeared. An older or
    // hand-edited registry deserializes `reasoning_levels` to an empty vec, so
    // the empty-array case is reachable: emit `[]`, never drop the key.
    let levels: Vec<Value> = if reasoning {
        model
            .reasoning_levels
            .iter()
            .map(|effort| json!({ "effort": effort, "description": format!("{effort} reasoning") }))
            .collect()
    } else {
        Vec::new()
    };
    object.insert("supported_reasoning_levels".into(), Value::Array(levels));
    // The optional default must name a level that actually exists. Core clears
    // `reasoning_levels` when reasoning is off, but a hand-edited registry may
    // not, so the capability gate is repeated here rather than assumed.
    match (reasoning, model.reasoning_levels.first()) {
        (true, Some(default)) => {
            object.insert(
                "default_reasoning_level".into(),
                Value::String(default.clone()),
            );
        }
        _ => {
            object.remove("default_reasoning_level");
        }
    }
    override_if_present(
        &mut object,
        "default_reasoning_summary",
        Value::String(if reasoning { "auto" } else { "none" }.into()),
    );
    override_if_present(
        &mut object,
        "supports_reasoning_summaries",
        Value::Bool(reasoning),
    );
    override_if_present(
        &mut object,
        "supports_reasoning_summary_parameter",
        Value::Bool(reasoning),
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

    /// The agent prompt a custom entry must end up carrying, trimmed to the part
    /// the tests care about. Shaped exactly like the real 0.156.0 catalog so
    /// `neutralize_model_identity` is exercised on real input.
    const OFFICIAL_INSTRUCTIONS: &str = "You are Codex, an agent based on GPT-6. You and the user \
share one workspace, and your job is to collaborate with them until their intended goal is \
completely handled.\n\n# When to ask the user for permission\n\nUse your best judgement.";

    /// A stub mirroring the **complete** field set of a real official entry, so
    /// "inherited" and "overridden" are distinguishable and a field the current
    /// Codex does not have cannot slip into a custom entry unnoticed. Values are
    /// the ones the 0.156.0 catalog uses for its modal model.
    fn official_entry(slug: &str, exclusive: bool) -> Value {
        let mut entry = json!({
            "slug": slug,
            "display_name": slug.to_uppercase(),
            "description": "official",
            "default_reasoning_level": "low",
            "default_reasoning_summary": "none",
            "supported_reasoning_levels": [{"effort": "low", "description": "low"}],
            "shell_type": "unified_exec",
            "base_instructions": OFFICIAL_INSTRUCTIONS,
            "model_messages": {
                "persistent_instructions": "persistent",
                "instructions_template": OFFICIAL_INSTRUCTIONS,
                "approvals": {"on_request_auto_review": "review text"},
                "token_budget": {"enabled": false}
            },
            // Harness contract. These are what an earlier revision nulled, which
            // silently cost custom models apply_patch and code mode.
            "tool_mode": "code_mode_only",
            "apply_patch_tool_type": "freeform",
            "multi_agent_version": "v2",
            "experimental_supported_tools": [],
            "use_responses_lite": true,
            "node_repl_disabled": false,
            "node_repl_auto_review_required": false,
            "truncation_policy": {"mode": "tokens", "limit": 10000},
            "effective_context_window_percent": 95,
            "comp_hash": "3000",
            "context_window": 272000,
            "max_context_window": 872000,
            // OpenAI-hosted / account-product fields.
            "supports_search_tool": true,
            "web_search_tool_type": "text_and_image",
            "support_verbosity": true,
            "default_verbosity": "low",
            "supports_image_detail_original": true,
            "supports_reasoning_effort_updates": false,
            "supports_experimental_context": false,
            "additional_speed_tiers": ["fast"],
            "service_tiers": [{"id": "priority", "name": "Fast"}],
            "availability_nux": null,
            "upgrade": null,
            "include_apps_usage_instructions": false,
            "include_plugin_usage_instructions": true,
            "include_skills_usage_instructions": false,
            "input_modalities": ["text", "image"],
            "visibility": "list",
            "supported_in_api": true,
            "priority": 1,
            "unknown_future_field": {"keep": true}
        });
        if exclusive {
            // Settings only one real model (gpt-6-astra) carries. `select_template`
            // must avoid picking this entry as the clone source.
            entry["node_repl_auto_review_required"] = json!(true);
            entry["supports_experimental_context"] = json!(true);
            entry["experimental_supported_tools"] = json!(["send_user_message_async", "clock"]);
            entry["multi_agent_reasoning_effort"] = json!("xhigh");
            entry["model_specialty"] = json!("cyber");
            entry["base_instructions"] = json!(format!("{OFFICIAL_INSTRUCTIONS} exclusive"));
            entry["model_messages"]["instructions_template"] =
                json!(format!("{OFFICIAL_INSTRUCTIONS} exclusive"));
        }
        entry
    }

    fn official() -> Value {
        json!({
            "models": [official_entry("gpt-5.6-sol", false)],
            "future_top_level": "keep"
        })
    }

    fn registry_with(
        dir: &std::path::Path,
        configure: impl FnOnce(&mut CustomModel),
    ) -> ProviderRegistry {
        let mut registry = ProviderRegistry::empty(dir.join("providers.json"));
        registry
            .add_provider(ProviderConfig::new("NewAPI", "https://example.test/v1").unwrap())
            .unwrap();
        let mut model = CustomModel::new("newapi", "qwen3.8", "NewAPI / Qwen3.8").unwrap();
        configure(&mut model);
        registry.add_model(model).unwrap();
        registry
    }

    fn tool_enabled_registry(dir: &std::path::Path) -> ProviderRegistry {
        registry_with(dir, |model| model.capabilities.tools = true)
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
        // Unknown fields survive the clone, which is the whole point of cloning an
        // official entry instead of constructing one from a local struct.
        assert_eq!(merged["models"][1]["unknown_future_field"]["keep"], true);
        // The official entry itself is never mutated.
        assert_eq!(merged["models"][0]["supports_search_tool"], true);
        assert_eq!(
            merged["models"][0]["model_messages"]["instructions_template"],
            OFFICIAL_INSTRUCTIONS
        );
    }

    /// P0 regression: an earlier revision wrote
    /// `instructions_template: "{{ instructions }}"`, which is not a placeholder
    /// Codex renders. Stock Codex 0.156.0 passed the literal 18-byte string
    /// through as the system prompt, so every custom model ran with no agent
    /// instructions at all — no apply_patch format, no shell conventions, no
    /// approval policy.
    #[test]
    fn custom_entry_carries_the_real_agent_instructions_not_a_placeholder() {
        let dir = tempdir().unwrap();
        let merged = merge_catalog(&official(), &tool_enabled_registry(dir.path())).unwrap();
        let custom = &merged["models"][1];

        let from_messages = custom["model_messages"]["instructions_template"]
            .as_str()
            .expect("instructions_template must be a string");
        let from_base = custom["base_instructions"]
            .as_str()
            .expect("base_instructions must be a string");

        for instructions in [from_messages, from_base] {
            assert!(
                instructions.len() > 100,
                "instructions must be the real prompt, got {} bytes: {instructions:?}",
                instructions.len()
            );
            assert!(
                !instructions.contains("{{"),
                "`{{ … }}` is not a Codex placeholder; it reaches the model verbatim"
            );
            assert!(
                instructions.contains("# When to ask the user for permission"),
                "the harness documentation must survive: {instructions:?}"
            );
            assert!(
                !instructions.contains("based on GPT"),
                "a third-party model must not be told it is a GPT: {instructions:?}"
            );
        }
        // Codex prefers `instructions_template`; both must agree so the
        // preference cannot change what the model is told.
        assert_eq!(from_messages, from_base);
    }

    /// The `model_messages` block carries the approval policy, collaboration
    /// modes, auto-review rules and token budget. Writing a two-key stub dropped
    /// all of it, leaving the model with tools it had no documentation for.
    #[test]
    fn custom_entry_inherits_the_whole_model_messages_policy_block() {
        let dir = tempdir().unwrap();
        let merged = merge_catalog(&official(), &tool_enabled_registry(dir.path())).unwrap();
        let messages = &merged["models"][1]["model_messages"];
        assert_eq!(messages["persistent_instructions"], "persistent");
        assert_eq!(
            messages["approvals"]["on_request_auto_review"],
            "review text"
        );
        assert_eq!(messages["token_budget"]["enabled"], false);
    }

    /// P0 regression: nulling `apply_patch_tool_type` left custom models with
    /// **no apply_patch tool at all**. It and the other fields below are
    /// client-side harness conventions Codex implements locally, not OpenAI-hosted
    /// services, so a third-party model needs them exactly as much as an official
    /// one does.
    ///
    /// `tool_mode` is conspicuously absent from the list — see the next test.
    #[test]
    fn a_tool_capable_model_inherits_the_full_harness_contract() {
        let dir = tempdir().unwrap();
        let merged = merge_catalog(&official(), &tool_enabled_registry(dir.path())).unwrap();
        let custom = &merged["models"][1];
        let official = &merged["models"][0];

        for field in [
            "apply_patch_tool_type",
            "multi_agent_version",
            "experimental_supported_tools",
            "use_responses_lite",
            "supports_experimental_context",
            "shell_type",
            "truncation_policy",
            "effective_context_window_percent",
            "comp_hash",
            "node_repl_disabled",
        ] {
            assert_eq!(
                custom[field], official[field],
                "`{field}` is a harness convention and must be inherited, not neutralised"
            );
        }
        assert_eq!(custom["apply_patch_tool_type"], "freeform");
    }

    /// P0 regression, second half — and a reversal of the first.
    ///
    /// Inheriting `tool_mode: "code_mode_only"` is not a harmless harness
    /// convention. Measured against stock Codex 0.156.0 over six catalog variants,
    /// it collapses the flat tool set (`exec_command`, `write_stdin`,
    /// `apply_patch`, `view_image`, the MCP resource tools, the goal tools) down to
    /// a single `custom:exec` tool whose own schema demands raw JavaScript for a
    /// fresh V8 isolate — "no Node, no file system, no network access, no console".
    /// No third-party model is trained on that surface, so it must stay off even
    /// when the model declares tool support, and the key must not be invented when
    /// the installed Codex does not have it.
    #[test]
    fn a_tool_capable_model_is_kept_off_the_code_mode_tool_surface() {
        let dir = tempdir().unwrap();
        let merged = merge_catalog(&official(), &tool_enabled_registry(dir.path())).unwrap();
        let custom = &merged["models"][1];

        assert_eq!(merged["models"][0]["tool_mode"], "code_mode_only");
        assert!(
            custom["tool_mode"].is_null(),
            "code mode must be neutralised for a tool-capable custom model, got {}",
            custom["tool_mode"]
        );
        // Neutralised, not removed: the field exists in this Codex build, and
        // `neutralize_field` never invents a key that the template lacks.
        assert!(
            custom.as_object().unwrap().contains_key("tool_mode"),
            "an existing template key must be nulled in place, not dropped"
        );
        assert_eq!(
            custom["apply_patch_tool_type"], "freeform",
            "apply_patch is independent of code mode and must survive"
        );
    }

    /// The panel's "supports tools" switch has to mean something: with tools off,
    /// the harness contract is neutralised in place so Codex advertises none.
    #[test]
    fn a_text_only_model_does_not_advertise_the_tool_harness() {
        let dir = tempdir().unwrap();
        let registry = registry_with(dir.path(), |model| model.capabilities.tools = false);
        let merged = merge_catalog(&official(), &registry).unwrap();
        let custom = &merged["models"][1];

        assert!(custom["tool_mode"].is_null(), "got {}", custom["tool_mode"]);
        assert!(
            custom["apply_patch_tool_type"].is_null(),
            "got {}",
            custom["apply_patch_tool_type"]
        );
        assert_eq!(
            custom["experimental_supported_tools"],
            json!([]),
            "must stay an array: Codex rejects null and discards the whole catalog"
        );
        assert_eq!(custom["use_responses_lite"], false);
    }

    /// A catalog from a Codex build that has never heard of code mode must not
    /// grow the key. `neutralize_field` returning early on an absent field is what
    /// keeps `custom_entry_creates_no_fields_the_official_catalog_lacks` honest.
    #[test]
    fn code_mode_fields_are_never_invented() {
        let dir = tempdir().unwrap();
        let mut official = official();
        official["models"][0]
            .as_object_mut()
            .unwrap()
            .remove("tool_mode");
        let merged = merge_catalog(&official, &tool_enabled_registry(dir.path())).unwrap();
        assert!(
            !merged["models"][1]
                .as_object()
                .unwrap()
                .contains_key("tool_mode"),
            "got {}",
            merged["models"][1]
        );
    }

    /// Task: the registry may replace the system prompt for one model, because the
    /// stock prompt documents a specific Codex build's tool grammar and some
    /// upstreams do better without it. The override must land in **both**
    /// instruction fields verbatim — Codex prefers `instructions_template`, and an
    /// override that only reached `base_instructions` would be silently ignored.
    #[test]
    fn a_registry_system_prompt_override_replaces_both_instruction_fields_verbatim() {
        let dir = tempdir().unwrap();
        let prompt = "You are a terse assistant.\nDo not invent files.";
        let registry = registry_with(dir.path(), |model| {
            model.system_prompt_override = Some(prompt.to_owned());
        });
        let merged = merge_catalog(&official(), &registry).unwrap();
        let custom = &merged["models"][1];

        assert_eq!(custom["base_instructions"], prompt);
        assert_eq!(custom["model_messages"]["instructions_template"], prompt);
        // Verbatim means verbatim: no identity rewriting of user-authored text.
        assert!(
            !custom["base_instructions"]
                .as_str()
                .unwrap()
                .contains("interactive coding agent")
        );
        // The rest of the policy block still comes from the template.
        assert_eq!(custom["model_messages"]["token_budget"]["enabled"], false);
        // The official entry must be untouched.
        assert_eq!(
            merged["models"][0]["base_instructions"],
            official()["models"][0]["base_instructions"]
        );
    }

    /// A blank override is "no override", not an empty system prompt — a blank
    /// prompt would leave the model with nothing telling it how to drive the tool
    /// loop it is still being offered. Cleared once by `normalize_system_prompt` in
    /// core and again by `custom_instructions`, so both a registry written through
    /// the API and one hand-edited behave the same.
    #[test]
    fn a_blank_system_prompt_override_falls_back_to_the_template() {
        let dir = tempdir().unwrap();
        let blank = registry_with(dir.path(), |model| {
            model.system_prompt_override = Some("   \n  ".to_owned());
        });
        let none = registry_with(dir.path(), |model| {
            model.system_prompt_override = None;
        });
        let blank_entry = merge_catalog(&official(), &blank).unwrap()["models"][1].clone();
        let none_entry = merge_catalog(&official(), &none).unwrap()["models"][1].clone();

        assert_eq!(blank_entry, none_entry, "a blank override must be a no-op");
        assert!(
            blank_entry["base_instructions"]
                .as_str()
                .unwrap()
                .starts_with("You are Codex, an interactive coding agent.")
        );
        assert!(
            blank_entry["model_messages"]["instructions_template"]
                .as_str()
                .unwrap()
                .len()
                > 100
        );
    }

    /// Hosted search is executed by the OpenAI backend. Advertising it makes
    /// Codex emit a `web_search` tool the upstream cannot run.
    #[test]
    fn custom_entry_never_advertises_hosted_services() {
        let dir = tempdir().unwrap();
        let merged = merge_catalog(&official(), &tool_enabled_registry(dir.path())).unwrap();
        assert_eq!(merged["models"][1]["supports_search_tool"], false);
        assert_eq!(merged["models"][0]["supports_search_tool"], true);
    }

    /// Auto-review routes a side request to the **official** `codex-auto-review`
    /// model. Leaving the template's value on would send a third-party
    /// conversation to OpenAI and bill it to the user's ChatGPT plan.
    #[test]
    fn custom_entry_cannot_trigger_official_auto_review() {
        let dir = tempdir().unwrap();
        // The exclusive entry has auto-review on; even if it were chosen as the
        // template the custom entry must turn it back off.
        let catalog = json!({
            "models": [official_entry("gpt-6-astra", true)]
        });
        let merged = merge_catalog(&catalog, &tool_enabled_registry(dir.path())).unwrap();
        assert_eq!(merged["models"][1]["node_repl_auto_review_required"], false);
    }

    /// Phantom fields are inert today but pin behaviour the moment Codex adds a
    /// real field of the same name, and they make the generated catalog differ
    /// from anything Codex itself would produce.
    #[test]
    fn custom_entry_creates_no_fields_the_official_catalog_lacks() {
        let dir = tempdir().unwrap();
        let merged = merge_catalog(&official(), &tool_enabled_registry(dir.path())).unwrap();
        let custom = merged["models"][1]
            .as_object()
            .expect("custom entry is an object");
        let official = merged["models"][0]
            .as_object()
            .expect("official entry is an object");

        let invented: Vec<&String> = custom
            .keys()
            .filter(|key| !official.contains_key(*key))
            .collect();
        assert!(
            invented.is_empty(),
            "custom entry invented fields no official entry has: {invented:?}"
        );
        // `model_specialty` describes an official model and must not be claimed.
        assert!(!custom.contains_key("model_specialty"));
    }

    /// Inheriting the template's 272 000 / 872 000 told Codex an 8k model had
    /// room for 272k tokens, so it packed the turn and the upstream rejected it.
    #[test]
    fn context_window_comes_from_the_registry_or_not_at_all() {
        let dir = tempdir().unwrap();
        let registry = registry_with(dir.path(), |model| {
            model.capabilities.tools = true;
            model.context_window = Some(8192);
        });
        let merged = merge_catalog(&official(), &registry).unwrap();
        assert_eq!(merged["models"][1]["context_window"], 8192);
        assert_eq!(merged["models"][1]["max_context_window"], 8192);

        let dir = tempdir().unwrap();
        let merged = merge_catalog(&official(), &tool_enabled_registry(dir.path())).unwrap();
        let custom = &merged["models"][1];
        assert!(
            custom.get("context_window").is_none(),
            "an undeclared window must fall back to Codex's own default, not the template's: {}",
            custom["context_window"]
        );
        assert!(custom.get("max_context_window").is_none());
    }

    /// Cloning the *first* official entry hands every custom model that entry's
    /// exclusives. Scoring by field-value agreement picks the modal profile.
    #[test]
    fn select_template_prefers_the_modal_entry_over_an_exclusive_one() {
        let catalog = json!({
            "models": [
                official_entry("gpt-6-astra", true),
                official_entry("gpt-5.6-sol", false),
                official_entry("gpt-5.6-terra", false),
                official_entry("gpt-5.6-luna", false)
            ]
        });
        let models = catalog["models"].as_array().unwrap();
        let template = select_template(models).expect("catalog has entries");
        assert_eq!(
            template["slug"], "gpt-5.6-sol",
            "ties must resolve to catalog order so the result is deterministic"
        );

        let dir = tempdir().unwrap();
        let merged = merge_catalog(&catalog, &tool_enabled_registry(dir.path())).unwrap();
        let custom = &merged["models"][4];
        assert_eq!(custom["slug"], "newapi/qwen3.8");
        assert_eq!(custom["node_repl_auto_review_required"], false);
        assert_eq!(custom["supports_experimental_context"], false);
        assert_eq!(custom["experimental_supported_tools"], json!([]));
        assert!(!custom.as_object().unwrap().contains_key("model_specialty"));
    }

    #[test]
    fn select_template_handles_degenerate_catalogs() {
        assert!(select_template(&[]).is_none());
        assert!(select_template(&[json!("not an object"), json!(3)]).is_none());
        let models = vec![json!({"slug": "only"})];
        assert_eq!(select_template(&models).unwrap()["slug"], "only");
    }

    /// `neutralize_model_identity` must be surgical: an upstream rewording has to
    /// degrade to "keep the official text", never to a mangled prompt.
    #[test]
    fn neutralize_model_identity_rewrites_only_the_identity_sentence() {
        assert_eq!(
            neutralize_model_identity("You are Codex, an agent based on GPT-6. Rest of prompt."),
            "You are Codex, an interactive coding agent. Rest of prompt."
        );
        // Model names contain dots; the boundary is ". ", not ".".
        assert_eq!(
            neutralize_model_identity("You are Codex, a coding agent based on GPT-5.6. Body."),
            "You are Codex, an interactive coding agent. Body."
        );
        // Unrecognised shapes are returned untouched.
        for untouched in [
            "You are a helpful assistant.",
            "You are Codex, an agent. No model named.",
            "Some other vendor's prompt based on GPT-6. Body.",
        ] {
            assert_eq!(
                neutralize_model_identity(untouched),
                untouched,
                "must not rewrite a prompt it does not recognise"
            );
        }
    }

    /// A template with no usable prompt must still yield an entry that carries
    /// one: Codex rejects an entry with neither `base_instructions` nor
    /// `model_messages`, and rejects the whole catalog with it.
    #[test]
    fn custom_instructions_falls_back_when_the_template_has_none() {
        let empty = json!({"slug": "x", "model_messages": {"instructions_template": "   "}});
        assert_eq!(
            custom_instructions(&empty, None),
            FALLBACK_CUSTOM_INSTRUCTIONS
        );
        assert_eq!(
            custom_instructions(&json!("not an object"), None),
            FALLBACK_CUSTOM_INSTRUCTIONS
        );
        // `base_instructions` is used when `model_messages` carries nothing.
        let base_only =
            json!({"base_instructions": "You are Codex, an agent based on GPT-6. Body."});
        assert_eq!(
            custom_instructions(&base_only, None),
            "You are Codex, an interactive coding agent. Body."
        );
        // An override beats every template path, including the fallback, and is
        // never identity-rewritten.
        assert_eq!(
            custom_instructions(
                &base_only,
                Some("You are Codex, an agent based on GPT-6. Body.")
            ),
            "You are Codex, an agent based on GPT-6. Body."
        );
        assert_eq!(
            custom_instructions(&json!("not an object"), Some("Override.")),
            "Override."
        );
        // Blank means "no override", so the template path still runs.
        assert_eq!(
            custom_instructions(&base_only, Some("  \n ")),
            "You are Codex, an interactive coding agent. Body."
        );
    }

    #[test]
    fn custom_entry_advertises_only_declared_capabilities() {
        let dir = tempdir().unwrap();
        let registry = registry_with(dir.path(), |model| {
            model.capabilities.tools = true;
            model.capabilities.images = true;
        });
        let merged = merge_catalog(&official(), &registry).unwrap();
        let custom = &merged["models"][1];
        assert_eq!(custom["input_modalities"], json!(["text", "image"]));
        assert_eq!(custom["shell_type"], "unified_exec");
        assert_eq!(custom["supports_search_tool"], false);
        assert!(custom["model_messages"].is_object());
        assert!(custom["base_instructions"].is_string());
    }

    #[test]
    fn custom_entry_advertises_all_declared_input_modalities_in_stable_order() {
        let dir = tempdir().unwrap();
        let registry = registry_with(dir.path(), |model| {
            model.capabilities.text = true;
            model.capabilities.images = true;
            model.capabilities.files = true;
            model.capabilities.audio = true;
            model.capabilities.video = true;
        });
        let merged = merge_catalog(&official(), &registry).unwrap();
        assert_eq!(
            merged["models"][1]["input_modalities"],
            json!(["text", "image", "file", "audio", "video"])
        );
    }

    #[test]
    fn reasoning_disabled_clears_catalog_reasoning_metadata() {
        let dir = tempdir().unwrap();
        let registry = registry_with(dir.path(), |model| {
            model.reasoning_levels = vec!["low".into(), "high".into()];
            model.capabilities.reasoning = false;
        });
        let merged = merge_catalog(&official(), &registry).unwrap();
        let custom = &merged["models"][1];
        assert_eq!(custom["supported_reasoning_levels"], json!([]));
        // The optional default must not name a level that is not offered. Core
        // clears `reasoning_levels` when reasoning is off, but a hand-edited
        // registry may not, so the gate is repeated in `custom_entry`.
        assert!(
            custom.get("default_reasoning_level").is_none(),
            "got {}",
            custom["default_reasoning_level"]
        );
        assert_eq!(custom["default_reasoning_summary"], "none");
    }

    #[test]
    fn reasoning_enabled_offers_the_registry_levels() {
        let dir = tempdir().unwrap();
        let registry = registry_with(dir.path(), |model| {
            model.capabilities.reasoning = true;
            model.reasoning_levels = vec!["low".into(), "high".into()];
        });
        let merged = merge_catalog(&official(), &registry).unwrap();
        let custom = &merged["models"][1];
        assert_eq!(
            custom["supported_reasoning_levels"],
            json!([
                {"effort": "low", "description": "low reasoning"},
                {"effort": "high", "description": "high reasoning"}
            ])
        );
        assert_eq!(custom["default_reasoning_level"], "low");
        assert_eq!(custom["default_reasoning_summary"], "auto");
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
        let registry = ProviderRegistry::empty(dir.path().join("providers.json"));
        let mut registry = registry;
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
        // `write_catalog_atomic` re-validates, so the merged output must pass the
        // same gate the official input did.
        validate_catalog(&merged).unwrap();
    }

    /// A template that omits the required-with-a-type fields entirely (a Codex
    /// build with a different schema, or a hand-written fixture) must still
    /// produce a valid entry — and must not grow fields that template never had.
    #[test]
    fn a_minimal_template_still_yields_a_valid_entry() {
        let dir = tempdir().unwrap();
        let catalog = json!({
            "models": [{
                "slug": "gpt-5.5",
                "shell_type": "unified_exec",
                "supported_reasoning_levels": [],
                "supports_search_tool": true,
                "experimental_supported_tools": [],
                "base_instructions": OFFICIAL_INSTRUCTIONS
            }]
        });
        let merged = merge_catalog(&catalog, &tool_enabled_registry(dir.path())).unwrap();
        validate_catalog(&merged).unwrap();
        assert_eq!(merged["models"][1]["shell_type"], "unified_exec");
        // Identity and picker metadata are the fields `custom_entry` must set
        // even when the template lacks them: without `visibility` and
        // `supported_in_api` the model is not selectable at all. Everything else
        // stays absent rather than being invented.
        let official = merged["models"][0].as_object().unwrap();
        let custom = merged["models"][1].as_object().unwrap();
        let mut invented: Vec<&str> = custom
            .keys()
            .filter(|key| !official.contains_key(*key))
            .map(String::as_str)
            .collect();
        invented.sort_unstable();
        let mut allowed = [
            "description",
            "display_name",
            "input_modalities",
            "priority",
            "supported_in_api",
            "visibility",
        ];
        allowed.sort_unstable();
        assert_eq!(invented, allowed);
    }

    /// A template whose `shell_type` is a map (the schema allows "string or map")
    /// must keep it rather than being flattened to a string.
    #[test]
    fn a_map_shell_type_is_inherited() {
        let dir = tempdir().unwrap();
        let mut catalog = official();
        catalog["models"][0]["shell_type"] = json!({"kind": "custom", "name": "zsh"});
        let merged = merge_catalog(&catalog, &tool_enabled_registry(dir.path())).unwrap();
        validate_catalog(&merged).unwrap();
        assert_eq!(
            merged["models"][1]["shell_type"],
            json!({"kind": "custom", "name": "zsh"})
        );

        // An empty string is not a usable value and must be replaced.
        let mut catalog = official();
        catalog["models"][0]["shell_type"] = json!("");
        let merged = merge_catalog(&catalog, &tool_enabled_registry(dir.path())).unwrap();
        assert_eq!(
            merged["models"][1]["shell_type"],
            FALLBACK_CUSTOM_SHELL_TYPE
        );
    }

    /// A model whose `reasoning_levels` is empty (an older registry, or a
    /// hand-edited one, deserializes to an empty vec) must still produce a valid
    /// entry: the fix is to emit an empty array, never to drop the required key.
    #[test]
    fn a_model_without_reasoning_levels_still_emits_the_required_field() {
        let dir = tempdir().unwrap();
        let registry = registry_with(dir.path(), |model| model.reasoning_levels = Vec::new());
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

    /// The bundled model catalog of a **real** Codex installation, captured
    /// verbatim from `codex debug models --bundled` on codex-cli 0.156.0.
    ///
    /// Refresh it with:
    /// `codex debug models --bundled > tests/fixtures/codex-<version>-bundled-catalog.json`
    /// (rename the old file in the same commit — `include_str!` below pins the
    /// path, so a rename that forgets it fails the build rather than the tests).
    ///
    /// `include_str!` rather than a runtime file read, so a fixture that goes
    /// missing or moves makes the test suite fail to *compile*. That is strictly
    /// better than a runtime skip, which is exactly the failure mode this replaces:
    /// these two checks used to run only against the hand-written stub — a stub
    /// with no `tool_mode` and no `apply_patch_tool_type` to inherit, which is how
    /// both P0s shipped — and CI ran a plain `cargo test --workspace` with neither
    /// `CODEX_MP_*` hook wired, so the "real Codex" path skipped on every push and
    /// still reported green.
    const COMMITTED_REAL_CATALOG: &str =
        include_str!("../../../tests/fixtures/codex-0.156.0-bundled-catalog.json");

    /// Proves in the CI log that the real-catalog assertions actually ran, without
    /// implying anything about which source was used.
    fn report_catalog_source(source: &str, catalog: &Value) {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            eprintln!(
                "real Codex catalog from {source} ({})",
                describe_catalog(catalog)
            );
        });
    }

    /// A real Codex catalog to assert against.
    ///
    /// Two opt-in overrides let the same checks be re-run against a newer Codex
    /// release without touching the fixture:
    ///
    /// 1. `CODEX_MP_CATALOG_FIXTURE` — a JSON dump.
    /// 2. `CODEX_MP_TEST_CODEX_BIN` — a Codex binary to query live.
    ///
    /// When either *is* set it must work; an explicitly requested real run that
    /// quietly fell back to the committed fixture would let a regression against the
    /// new release pass. With neither set the committed fixture is used, so there is
    /// no path through this function that runs no real-catalog assertions at all.
    fn real_catalog() -> Value {
        if let Some(path) = std::env::var_os("CODEX_MP_CATALOG_FIXTURE").map(PathBuf::from) {
            let text = fs::read_to_string(&path).unwrap_or_else(|error| {
                panic!(
                    "CODEX_MP_CATALOG_FIXTURE={} is not readable: {error}",
                    path.display()
                )
            });
            let catalog = parse_catalog(&text, &format!("fixture {}", path.display()));
            report_catalog_source(&format!("fixture {}", path.display()), &catalog);
            return catalog;
        }
        if let Some(requested) = std::env::var_os("CODEX_MP_TEST_CODEX_BIN").map(PathBuf::from) {
            // Resolve through `PATH` before asserting. `Path::is_file` on a bare
            // name like `codex` is CWD-relative, so the documented and CI-used form
            // `CODEX_MP_TEST_CODEX_BIN=codex` failed here even though
            // `discover_official_catalog` would have found it — `command_for_executable`
            // resolves through the same `PATH` helper this check was skipping.
            let codex_bin = codex_mp_core::resolve_executable(&requested);
            assert!(
                codex_bin.is_file(),
                "CODEX_MP_TEST_CODEX_BIN={} was not found (nor on PATH)",
                requested.display()
            );
            let catalog = discover_official_catalog(&codex_bin).unwrap_or_else(|error| {
                panic!("`{} debug models` failed: {error}", codex_bin.display())
            });
            report_catalog_source(&codex_bin.display().to_string(), &catalog);
            return catalog;
        }
        let catalog = parse_catalog(COMMITTED_REAL_CATALOG, "committed fixture codex-0.156.0");
        report_catalog_source("committed fixture codex-0.156.0", &catalog);
        catalog
    }

    fn parse_catalog(text: &str, source: &str) -> Value {
        serde_json::from_str(text)
            .unwrap_or_else(|error| panic!("{source} is not valid JSON: {error}"))
    }

    /// Model count and first slug, for the CI log line that proves these
    /// assertions actually ran against something real.
    fn describe_catalog(catalog: &Value) -> String {
        match catalog["models"].as_array() {
            Some(models) => format!(
                "{} models, first slug {:?}",
                models.len(),
                models.first().and_then(|model| model["slug"].as_str())
            ),
            None => "no models array".to_owned(),
        }
    }

    #[test]
    fn command_output_is_current_installation_source() {
        let catalog = real_catalog();
        validate_catalog(&catalog).unwrap();
        let models = catalog["models"].as_array().expect("validated catalog");
        assert!(
            !models.is_empty(),
            "a real Codex catalog always advertises at least one model"
        );
        // Every slug must be usable as a logical model id, and none may collide
        // with the `provider/model` shape the registry produces.
        for model in models {
            let slug = model["slug"].as_str().expect("slug is a string");
            assert!(!slug.is_empty());
            assert!(
                !slug.contains('/'),
                "`{slug}` would collide with a custom logical model id"
            );
        }
    }

    /// End-to-end check of the generated catalog against a **real** Codex
    /// catalog, rather than the hand-written stub the other tests use. The two
    /// P0s this covers were invisible to stub-based tests: the stub did not have
    /// `tool_mode` / `apply_patch_tool_type` to inherit, and its prompt was too
    /// short for a placeholder to be obviously wrong.
    #[test]
    fn real_catalog_snapshot_round_trips_with_custom_entry() {
        let official = real_catalog();
        validate_catalog(&official).unwrap();
        let official_count = official["models"].as_array().unwrap().len();

        let dir = tempdir().unwrap();
        let merged = merge_catalog(&official, &tool_enabled_registry(dir.path())).unwrap();
        let encoded = serde_json::to_vec(&merged).unwrap();
        let reparsed: Value = serde_json::from_slice(&encoded).unwrap();
        validate_catalog(&reparsed).unwrap();

        let models = reparsed["models"].as_array().unwrap();
        assert_eq!(models.len(), official_count + 1);
        // Official entries are passed through byte-for-byte; only appended.
        let official_models = official["models"].as_array().unwrap();
        assert_eq!(models[..official_count], official_models[..]);
        let custom = &models[official_count];
        assert_eq!(custom["slug"], "newapi/qwen3.8");

        // P0-1: the prompt must be the real agent instructions, not a placeholder.
        let instructions = custom["model_messages"]["instructions_template"]
            .as_str()
            .or_else(|| custom["base_instructions"].as_str())
            .expect("a custom entry always carries instructions");
        assert!(
            instructions.len() > 1000,
            "the real Codex prompt is tens of thousands of characters; got {}",
            instructions.len()
        );
        assert!(
            !instructions.contains("{{"),
            "`{{ … }}` is not a Codex placeholder and reaches the model verbatim"
        );
        assert!(
            !instructions.contains("based on GPT"),
            "a third-party model must not be told it is a GPT"
        );

        // P0-2: the harness contract must survive, or the model gets no
        // apply_patch and cannot edit files through the tool loop.
        let template = select_template(official["models"].as_array().unwrap()).unwrap();
        for field in [
            "apply_patch_tool_type",
            "shell_type",
            "multi_agent_version",
            "use_responses_lite",
            "truncation_policy",
            "effective_context_window_percent",
            "comp_hash",
        ] {
            assert_eq!(
                custom[field], template[field],
                "`{field}` must be inherited from the official template"
            );
        }
        // P0-2b: the one harness-looking field that must NOT be inherited. Stock
        // 0.156.0 sets `tool_mode: "code_mode_only"`, which replaces the flat tool
        // set with a single JavaScript-in-V8 `exec` tool no third-party model can
        // drive. Asserted against the real catalog so a future Codex that renames
        // or drops the field is caught here rather than in the field.
        for field in CODE_MODE_TOOL_FIELDS {
            if template.get(field).is_some() {
                assert!(
                    custom[field].is_null(),
                    "`{field}` must be neutralised; template had {}, custom has {}",
                    template[field],
                    custom[field]
                );
            } else {
                assert!(
                    !custom.as_object().unwrap().contains_key(*field),
                    "`{field}` must not be invented when this Codex build lacks it"
                );
            }
        }
        assert_eq!(custom["supports_search_tool"], false);
        assert_eq!(custom["node_repl_auto_review_required"], false);

        // No field may be invented: the generated catalog has to look like
        // something Codex itself would produce.
        let template_keys = template.as_object().unwrap();
        let invented: Vec<&String> = custom
            .as_object()
            .unwrap()
            .keys()
            .filter(|key| !template_keys.contains_key(*key))
            .collect();
        assert!(
            invented.is_empty(),
            "custom entry invented fields the real catalog does not have: {invented:?}"
        );
        // The context window is undeclared in this registry, so neither key may
        // carry the template's value.
        assert!(custom.get("context_window").is_none());
        assert!(custom.get("max_context_window").is_none());

        // What `sync` actually writes must round-trip.
        let path = dir.path().join("models.json");
        write_catalog_atomic(&reparsed, &path).unwrap();
        let on_disk: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk, reparsed);
        assert_eq!(
            schema_fingerprint(&on_disk).unwrap(),
            schema_fingerprint(&official).unwrap()
        );
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
