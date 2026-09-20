//! Management of official Codex / ChatGPT accounts, credentials persistence,
//! token refresh, rate limits usage checking, atomic switching, and Codex process restarting.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use codex_mp_core::FileLock;
use codex_mp_credentials::{CredentialStore, CredentialStoreError, NativeCredentialStore};
use directories::{BaseDirs, ProjectDirs};
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Keyring service name under which managed account tokens are stored.
const ACCOUNT_CREDENTIAL_SERVICE: &str = "dev.codex-multiprovider.accounts";

/// Only this many `auth.bak-switch-*` copies are kept. Each one contains live
/// OAuth tokens, so an unbounded pile of them is a credential-leak hazard.
const MAX_AUTH_BACKUPS: usize = 3;

/// Credential reference for a managed account's token set.
fn account_credential_reference(id: &str) -> String {
    format!("account:{id}")
}

/// Whether a stored account corresponds to the account Codex is currently logged
/// in as. Prefers the account id, falling back to the e-mail because older Codex
/// builds did not always persist `account_id`.
fn account_matches_active(account: &ManagedAccount, active: &ActiveAccountStatus) -> bool {
    if let Some(matched_id) = &active.matched_account_id {
        return account.id == *matched_id;
    }
    match (&account.email, &active.email) {
        (Some(account_email), Some(active_email)) => {
            account_email.eq_ignore_ascii_case(active_email)
        }
        _ => false,
    }
}

const OPENAI_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Largest body accepted from the (hard-coded, official) token and usage
/// endpoints.
///
/// `Response::json()` buffers the whole body with no limit. These endpoints are
/// HTTPS and not user-controlled, so this is defence in depth rather than a
/// reachable attack — but a captive portal or a broken intermediary could still
/// return something enormous, and the same bound is already applied to provider
/// discovery.
const MAX_ACCOUNT_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Read an error body for a diagnostic message, truncated to a readable size.
///
/// `text()` buffers the whole body; an error path only ever shows a short
/// snippet, so an oversized or hostile response should not be absorbed whole.
async fn read_bounded_error_text(response: reqwest::Response) -> String {
    /// Enough for a readable diagnostic, far below any response worth buffering.
    const MAX_ERROR_BODY_BYTES: usize = 8 * 1024;
    let text = response.text().await.unwrap_or_default();
    if text.len() <= MAX_ERROR_BODY_BYTES {
        return text;
    }
    let mut truncated: String = text.chars().take(MAX_ERROR_BODY_BYTES).collect();
    truncated.push_str("… (truncated)");
    truncated
}

/// Read a bounded JSON body, rejecting anything larger than the cap.
async fn read_bounded_account_json(
    response: reqwest::Response,
    what: &str,
) -> Result<serde_json::Value, AccountError> {
    if let Some(length) = response.content_length()
        && length > MAX_ACCOUNT_RESPONSE_BYTES as u64
    {
        return Err(AccountError::RefreshFailed(format!(
            "{what} returned {length} bytes, exceeding the {MAX_ACCOUNT_RESPONSE_BYTES} byte limit"
        )));
    }
    let text = response.text().await?;
    if text.len() > MAX_ACCOUNT_RESPONSE_BYTES {
        return Err(AccountError::RefreshFailed(format!(
            "{what} returned {} bytes, exceeding the {MAX_ACCOUNT_RESPONSE_BYTES} byte limit",
            text.len()
        )));
    }
    serde_json::from_str(&text).map_err(AccountError::from)
}

/// Extract only the non-secret OAuth error fields. The complete response is
/// still bounded by `read_bounded_error_text`, but persisting or returning the
/// raw body would make the health state noisy and could expose intermediary
/// diagnostics to the browser.
fn oauth_error_details(body: &str) -> (Option<String>, String) {
    let parsed = serde_json::from_str::<serde_json::Value>(body).ok();
    let nested = parsed.as_ref().and_then(|value| value.get("error"));
    let source = nested.or(parsed.as_ref());
    let code = source
        .and_then(|value| value.get("code"))
        .and_then(|value| value.as_str())
        .map(str::to_owned);
    let message = source
        .and_then(|value| value.get("message"))
        .and_then(|value| value.as_str())
        .unwrap_or(body)
        .chars()
        .take(256)
        .collect();
    (code, message)
}

fn is_reauthentication_code(code: Option<&str>, status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::UNAUTHORIZED
        || code.is_some_and(|value| {
            matches!(
                value,
                "refresh_token_expired"
                    | "refresh_token_reused"
                    | "refresh_token_invalidated"
                    | "refresh_token_account_mismatch"
            )
        })
}

const OPENAI_OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const OPENAI_WHAM_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const DEFAULT_USER_AGENT: &str = "codex_cli_rs";

#[derive(Debug, Error)]
pub enum AccountError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// The accounts store exists but cannot be parsed.
    ///
    /// Reported with the file path and an explicit remedy so a user whose store
    /// was corrupted by a hand-edit or a bad write can recover, instead of seeing
    /// a bare parser message from the panel.
    #[error(
        "the accounts store at {path} is not valid JSON ({detail}); \
         move it aside (for example `mv {path} {path}.bak`) to start from an empty \
         list, then re-import your accounts"
    )]
    StoreUnreadable { path: String, detail: String },
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("Account not found: `{0}`")]
    NotFound(String),
    #[error("Invalid token format: {0}")]
    InvalidToken(String),
    #[error("Account `{0}` has no stored credentials; refusing to overwrite auth.json")]
    MissingCredentials(String),
    #[error("Account `{0}` requires re-authentication before it can be activated")]
    RequiresReauthentication(String),
    #[error("Token refresh failed: {0}")]
    RefreshFailed(String),
    #[error("Account `{account_id}` requires re-authentication ({code}): {message}")]
    ReauthenticationRequired {
        account_id: String,
        code: String,
        message: String,
    },
    #[error("Account identity changed during token refresh: {0}")]
    AccountIdentityMismatch(String),
    #[error("Rate limit check failed: HTTP {status} {message}")]
    UsageCheckFailed { status: u16, message: String },
    #[error("Failed to determine Codex home directory")]
    CodexHomeNotFound,
    #[error("Failed to parse auth.json: {0}")]
    AuthJsonInvalid(String),
    #[error("Credential store error: {0}")]
    Credential(#[from] CredentialStoreError),
}

impl AccountError {
    /// Stable, secret-free error code for the account API and persisted health
    /// state. The display implementation intentionally remains more detailed
    /// for local diagnostics, while the web surface uses this method.
    pub fn public_code(&self) -> &'static str {
        match self {
            Self::ReauthenticationRequired { .. }
            | Self::AccountIdentityMismatch(_)
            | Self::RequiresReauthentication(_) => "reauth_required",
            Self::UsageCheckFailed { status: 401, .. } => "usage_unauthorized",
            Self::UsageCheckFailed { status: 429, .. } => "usage_rate_limited",
            Self::UsageCheckFailed { .. } => "usage_check_failed",
            Self::Network(_) => "network_error",
            Self::RefreshFailed(_) => "refresh_failed",
            _ => "account_operation_failed",
        }
    }

    /// Human-readable message safe to persist in the account metadata and send
    /// to the panel. It never includes the OAuth endpoint response body.
    pub fn public_message(&self) -> String {
        match self {
            Self::ReauthenticationRequired { .. }
            | Self::AccountIdentityMismatch(_)
            | Self::RequiresReauthentication(_) => {
                "官方账号凭证已失效或身份不一致，请重新登录后再次保存当前账号。".into()
            }
            Self::UsageCheckFailed { status: 401, .. } => {
                "额度服务拒绝了当前凭证，请重新登录后重试。".into()
            }
            Self::UsageCheckFailed { status: 429, .. } => "额度服务暂时限流，请稍后重试。".into(),
            Self::Network(_) => "额度服务网络请求失败，请检查网络后重试。".into(),
            Self::RefreshFailed(_) => "官方账号凭证刷新失败，请稍后重试。".into(),
            _ => self.to_string(),
        }
    }
}

/// Token payload stored in auth.json and account store
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountTokens {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

impl AccountTokens {
    /// True when no secret material is present (i.e. nothing to persist).
    pub fn is_empty(&self) -> bool {
        self.id_token.is_none() && self.access_token.is_none() && self.refresh_token.is_none()
    }

    /// A token bundle with only an id token cannot be used by Codex or the
    /// usage endpoint. Keep the account record from becoming a logout payload.
    pub fn has_usable_credentials(&self) -> bool {
        self.access_token.is_some() || self.refresh_token.is_some()
    }

    fn credential_state(&self) -> CredentialState {
        if self.refresh_token.is_some() {
            CredentialState::Ready
        } else if self.access_token.is_some() {
            CredentialState::AccessOnly
        } else {
            CredentialState::Unknown
        }
    }
}

/// Parsed profile claim info from id_token
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ProfileInfo {
    pub email: Option<String>,
    pub name: Option<String>,
    pub sub: Option<String>,
    pub plan_type: Option<String>,
    pub user_id: Option<String>,
    pub chatgpt_account_id: Option<String>,
}

/// Token Window info for usage
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct UsageWindow {
    pub used_percent: u32,
    pub limit_window_seconds: u64,
    pub reset_after_seconds: u64,
    pub reset_at: u64,
}

/// Rate limit structure returned by Wham usage API
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct RateLimitSummary {
    pub allowed: bool,
    pub limit_reached: bool,
    pub primary_window: Option<UsageWindow>,
    pub secondary_window: Option<UsageWindow>,
}

/// Reserve limit structure
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ReserveLimitSummary {
    pub limit_name: String,
    pub allowed: bool,
    pub limit_reached: bool,
    pub used_percent: Option<u32>,
    pub reset_after_seconds: Option<u64>,
    pub reset_at: Option<u64>,
}

/// Complete parsed quota/usage snapshot for an account
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AccountUsageSnapshot {
    pub updated_at: u64,
    pub plan_type: Option<String>,
    pub primary_5h: Option<UsageWindow>,
    pub secondary_weekly: Option<UsageWindow>,
    pub reserve: Option<ReserveLimitSummary>,
}

/// Health of the credential owned by the account manager. This is deliberately
/// separate from whether Codex's live auth.json is currently active.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CredentialState {
    Ready,
    AccessOnly,
    NeedsReauth,
    #[default]
    Unknown,
}

/// Secret-free persisted diagnostic attached to an account or usage query.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountIssue {
    pub code: String,
    pub message: String,
    pub occurred_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsageStatus {
    Unknown,
    Fresh,
    Stale,
    ReauthRequired,
    Error,
}

/// Managed account entry
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedAccount {
    pub id: String,
    pub name: String,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub user_id: Option<String>,
    pub account_id: Option<String>,
    /// Secrets are never written to the on-disk store; they live in the OS
    /// keyring. `skip_serializing` keeps them out of the JSON while still
    /// allowing a legacy store file (which embedded them) to be read and
    /// migrated on first load.
    #[serde(default, skip_serializing)]
    pub tokens: AccountTokens,
    pub last_refresh: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub usage: Option<AccountUsageSnapshot>,
    #[serde(default)]
    pub credential_state: CredentialState,
    #[serde(default)]
    pub credential_issue: Option<AccountIssue>,
    #[serde(default)]
    pub usage_issue: Option<AccountIssue>,
}

/// Persistent store file structure
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AccountsFile {
    pub schema_version: u32,
    pub accounts: Vec<ManagedAccount>,
}

impl ManagedAccount {
    /// Secret-free projection of this account.
    ///
    /// Every API response and UI path must go through this: `ManagedAccount`
    /// carries `tokens`, including the long-lived OAuth refresh token, and must
    /// never be serialized to a client.
    pub fn to_summary(&self, is_active: bool) -> AccountSummary {
        let usage_status = if let Some(issue) = &self.usage_issue {
            if issue.code == "reauth_required" {
                UsageStatus::ReauthRequired
            } else if self.usage.is_some() {
                UsageStatus::Stale
            } else {
                UsageStatus::Error
            }
        } else if self.usage.is_some() {
            UsageStatus::Fresh
        } else {
            UsageStatus::Unknown
        };
        AccountSummary {
            id: self.id.clone(),
            name: self.name.clone(),
            email: self.email.clone(),
            plan_type: self.plan_type.clone(),
            is_active,
            updated_at: self.updated_at,
            usage: self.usage.clone(),
            credential_state: self.credential_state.clone(),
            credential_issue: self.credential_issue.clone(),
            usage_status,
            usage_issue: self.usage_issue.clone(),
        }
    }
}

/// Public summary for Web / UI
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountSummary {
    pub id: String,
    pub name: String,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub is_active: bool,
    pub updated_at: u64,
    pub usage: Option<AccountUsageSnapshot>,
    pub credential_state: CredentialState,
    pub credential_issue: Option<AccountIssue>,
    pub usage_status: UsageStatus,
    pub usage_issue: Option<AccountIssue>,
}

/// Active account detection result
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveAccountStatus {
    pub is_logged_in: bool,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub account_id: Option<String>,
    pub matched_account_id: Option<String>,
    pub auth_mode: Option<String>,
}

pub struct AccountManager {
    store_path: PathBuf,
    codex_home: PathBuf,
    // `Arc` so the whole manager can be moved into `spawn_blocking`: every one of
    // its synchronous methods may touch the OS keyring, which must never run on
    // an async worker thread (see `codex_mp_credentials::run_blocking`).
    http_client: Arc<Client>,
    credentials: Arc<dyn CredentialStore>,
    refresh_locks: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

impl AccountManager {
    pub fn new() -> Result<Self, AccountError> {
        let store_path = default_accounts_path();
        let codex_home = default_codex_home().ok_or(AccountError::CodexHomeNotFound)?;
        Ok(Self::with_paths(store_path, codex_home))
    }

    pub fn with_paths(store_path: impl Into<PathBuf>, codex_home: impl Into<PathBuf>) -> Self {
        Self::with_credential_store(
            store_path,
            codex_home,
            Arc::new(NativeCredentialStore::new(ACCOUNT_CREDENTIAL_SERVICE)),
        )
    }

    /// Build a manager backed by a caller-supplied credential store. Tests use
    /// this to avoid touching the host keyring.
    pub fn with_credential_store(
        store_path: impl Into<PathBuf>,
        codex_home: impl Into<PathBuf>,
        credentials: Arc<dyn CredentialStore>,
    ) -> Self {
        Self {
            store_path: store_path.into(),
            codex_home: codex_home.into(),
            http_client: Arc::new(
                Client::builder()
                    // This client POSTs a **live OAuth refresh token** to the
                    // token endpoint. Without an explicit policy reqwest uses
                    // `Policy::limited(10)`, and a 307/308 redirect makes it
                    // resend the request body — the refresh token — to whatever
                    // host the redirect names. Verified with a local redirector:
                    // the target received `refresh_token=SECRET-RT`.
                    //
                    // Every other client in this project already sets this; this
                    // was the one that carries the most sensitive payload.
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(Duration::from_secs(15))
                    .build()
                    .unwrap_or_else(|_| {
                        eprintln!(
                            "codex-mp: FATAL: no hardened account client available; \
                             OAuth redirects will be followed"
                        );
                        Client::new()
                    }),
            ),
            credentials,
            refresh_locks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn store_path(&self) -> &Path {
        &self.store_path
    }

    pub fn codex_home(&self) -> &Path {
        &self.codex_home
    }

    pub fn auth_json_path(&self) -> PathBuf {
        self.codex_home.join("auth.json")
    }

    /// One in-process flight per account. The accounts-file lock acquired by
    /// `refresh_account_token` extends this protection across multiple
    /// OmniBridge processes as well.
    fn refresh_lock_for(&self, account_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self
            .refresh_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        locks
            .entry(account_id.to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    // ---------------- Persistence ---------------- //

    /// Load the accounts store while holding the cross-process lock.
    ///
    /// Callers that mutate must use this and keep the guard alive until after
    /// [`Self::save_file`]. `load_file` + `save_file` on its own is a lost-update
    /// race: two writers can each read revision N and both replace it, silently
    /// dropping one account. This was reproducible with 8 concurrent imports.
    pub fn load_file_locked(&self) -> Result<(AccountsFile, FileLock), AccountError> {
        let guard = FileLock::acquire(&self.store_path)?;
        let file = self.load_file()?;
        Ok((file, guard))
    }

    pub fn load_file(&self) -> Result<AccountsFile, AccountError> {
        if !self.store_path.exists() {
            return Ok(AccountsFile {
                schema_version: 1,
                accounts: Vec::new(),
            });
        }
        let content = fs::read_to_string(&self.store_path)?;
        // Name the file and the remedy. A bare `expected value at line 1 column 15`
        // gave the user no way to know *which* file was broken, and the panel
        // surfaced it verbatim — leaving account import and listing both failing
        // with no recovery path other than guessing.
        let mut file: AccountsFile =
            serde_json::from_str(&content).map_err(|error| AccountError::StoreUnreadable {
                path: self.store_path.display().to_string(),
                detail: error.to_string(),
            })?;

        // Tokens are keyring-resident. A store written by an older release still
        // carries them inline, so re-home those into the keyring now and rewrite
        // the file without them.
        let mut migrated_legacy_secrets = false;
        for account in &mut file.accounts {
            let reference = account_credential_reference(&account.id);
            if !account.tokens.is_empty() {
                if account.credential_state == CredentialState::Unknown {
                    account.credential_state = account.tokens.credential_state();
                }
                let secret = SecretString::from(serde_json::to_string(&account.tokens)?);
                // Attempt migration to keyring/credential store. If migration fails (e.g. platform limits or backend issues),
                // do NOT fail the entire load_file: keep the inline tokens in memory so the accounts can still be used.
                if let Err(err) = self.credentials.set(&reference, &secret) {
                    eprintln!(
                        "codex-mp: failed to migrate inline tokens for account `{}` to credentials store: {err}",
                        account.id
                    );
                } else {
                    migrated_legacy_secrets = true;
                }
                continue;
            }
            match self.credentials.get(&reference) {
                Ok(secret) => {
                    account.tokens = serde_json::from_str::<AccountTokens>(secret.expose_secret())
                        .map_err(|error| {
                            AccountError::InvalidToken(format!(
                                "stored credentials for account `{}` are unreadable: {error}",
                                account.id
                            ))
                        })?;
                }
                // Genuinely nothing stored: this account has no credentials yet.
                Err(CredentialStoreError::NotFound(_)) => {}
                // The backend failed. Treating that as "no credentials" used to
                // let a later `switch_to_account` write an empty token set into
                // `auth.json`, destroying the user's live session. Fail closed.
                Err(error) => return Err(AccountError::Credential(error)),
            }
            if account.credential_state == CredentialState::Unknown {
                account.credential_state = account.tokens.credential_state();
            }
        }
        if migrated_legacy_secrets {
            self.save_file(&file)?;
        }

        Ok(file)
    }

    /// Read only the non-secret account metadata. Active-session detection
    /// must remain available even when an unrelated managed credential is
    /// temporarily unreadable in the OS keyring.
    fn load_metadata_file(&self) -> Result<AccountsFile, AccountError> {
        if !self.store_path.exists() {
            return Ok(AccountsFile {
                schema_version: 1,
                accounts: Vec::new(),
            });
        }
        let content = fs::read_to_string(&self.store_path)?;
        serde_json::from_str(&content).map_err(|error| AccountError::StoreUnreadable {
            path: self.store_path.display().to_string(),
            detail: error.to_string(),
        })
    }

    /// Persist the accounts document.
    ///
    /// Only credentials that actually changed are written. Writing every
    /// account's tokens on every save was both wasteful and fragile: a usage
    /// refresh for *one* account rewrote *all* of them, so a keyring problem on
    /// any single account failed an unrelated account's refresh.
    pub fn save_file(&self, file: &AccountsFile) -> Result<(), AccountError> {
        // Persist secrets to the keyring before the metadata document, so a
        // failure cannot leave an account whose tokens were never stored.
        for account in &file.accounts {
            if account.tokens.is_empty() {
                continue;
            }
            let reference = account_credential_reference(&account.id);
            let secret = SecretString::from(serde_json::to_string(&account.tokens)?);
            // Skip accounts whose stored secret already matches. A keyring
            // round-trip is not free, and this makes a usage-only update touch no
            // credentials at all.
            if let Ok(existing) = self.credentials.get(&reference)
                && existing.expose_secret() == secret.expose_secret()
            {
                continue;
            }
            self.credentials.set(&reference, &secret)?;
        }

        if let Some(parent) = self.store_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut bytes = serde_json::to_vec_pretty(file)?;
        bytes.push(b'\n');
        // Private from the moment it exists: this file names the accounts, and a
        // `File::create` temp file is umask-readable (0664 here) until the chmod.
        codex_mp_core::write_private_atomic(&self.store_path, &bytes)?;
        Ok(())
    }

    // ---------------- Reading Active Auth ---------------- //

    pub fn read_active_auth(&self) -> Result<Option<serde_json::Value>, AccountError> {
        let path = self.auth_json_path();
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(&path)?;
        let val: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| AccountError::AuthJsonInvalid(e.to_string()))?;
        Ok(Some(val))
    }

    pub fn check_active_status(&self) -> Result<ActiveAccountStatus, AccountError> {
        let active_auth = match self.read_active_auth()? {
            Some(auth) => auth,
            None => {
                return Ok(ActiveAccountStatus {
                    is_logged_in: false,
                    email: None,
                    plan_type: None,
                    account_id: None,
                    matched_account_id: None,
                    auth_mode: None,
                });
            }
        };

        let auth_mode = active_auth
            .get("auth_mode")
            .and_then(|v| v.as_str())
            .map(String::from);

        // `auth.json` can also contain an API-key or an incomplete/old
        // document. Its mere presence must not make the panel claim that an
        // official OAuth session is active.
        if auth_mode.as_deref() != Some("chatgpt") {
            return Ok(ActiveAccountStatus {
                is_logged_in: false,
                email: None,
                plan_type: None,
                account_id: None,
                matched_account_id: None,
                auth_mode,
            });
        }

        let tokens_val = active_auth.get("tokens");
        let tokens = tokens_val
            .cloned()
            .ok_or_else(|| AccountError::AuthJsonInvalid("tokens field missing".into()))
            .and_then(|value| {
                serde_json::from_value::<AccountTokens>(value).map_err(|error| {
                    AccountError::AuthJsonInvalid(format!("invalid tokens: {error}"))
                })
            })?;
        if !tokens.has_usable_credentials() {
            return Ok(ActiveAccountStatus {
                is_logged_in: false,
                email: None,
                plan_type: None,
                account_id: None,
                matched_account_id: None,
                auth_mode,
            });
        }
        let id_token = tokens_val
            .and_then(|t| t.get("id_token"))
            .and_then(|v| v.as_str());
        let account_id = tokens_val
            .and_then(|t| t.get("account_id"))
            .and_then(|v| v.as_str())
            .map(String::from);

        let profile = id_token
            .and_then(|tok| parse_id_token_claims(tok).ok())
            .unwrap_or_default();

        let email = profile.email;
        let plan_type = profile.plan_type;
        let effective_account_id = account_id.or(profile.chatgpt_account_id);

        if email.is_none() && effective_account_id.is_none() {
            return Ok(ActiveAccountStatus {
                is_logged_in: false,
                email: None,
                plan_type: None,
                account_id: None,
                matched_account_id: None,
                auth_mode,
            });
        }

        let file = self.load_metadata_file()?;
        // Account id is the stronger identity. Only fall back to e-mail when
        // the active auth has no account id, or when the stored legacy record
        // has no id of its own. This prevents two saved accounts sharing an
        // e-mail from selecting the wrong live token bundle.
        let matched = if let Some(current_account_id) = &effective_account_id {
            file.accounts
                .iter()
                .find(|account| account.account_id.as_ref() == Some(current_account_id))
                .or_else(|| {
                    file.accounts.iter().find(|account| {
                        account.account_id.is_none()
                            && matches!((&account.email, &email), (Some(a), Some(e)) if a.eq_ignore_ascii_case(e))
                    })
                })
        } else {
            file.accounts.iter().find(|account| {
                matches!((&account.email, &email), (Some(a), Some(e)) if a.eq_ignore_ascii_case(e))
            })
        };

        Ok(ActiveAccountStatus {
            is_logged_in: true,
            email,
            plan_type,
            account_id: effective_account_id,
            matched_account_id: matched.map(|m| m.id.clone()),
            auth_mode,
        })
    }

    // ---------------- Account List & CRUD ---------------- //

    pub fn list_accounts(&self) -> Result<Vec<AccountSummary>, AccountError> {
        let file = self.load_file()?;
        let active = self.check_active_status()?;

        let mut summaries = Vec::new();
        for acc in file.accounts {
            summaries.push(acc.to_summary(account_matches_active(&acc, &active)));
        }

        Ok(summaries)
    }

    /// Secret-free summary of a single account, including whether it is active.
    ///
    /// Use this instead of returning a `ManagedAccount` (and therefore its
    /// `tokens`) from any API surface.
    pub fn summarise_account(&self, account: &ManagedAccount) -> AccountSummary {
        let is_active = self
            .check_active_status()
            .map(|active| account_matches_active(account, &active))
            .unwrap_or(false);
        account.to_summary(is_active)
    }

    /// Capture current active auth.json into managed accounts
    pub fn capture_current_auth(
        &self,
        custom_name: Option<String>,
    ) -> Result<ManagedAccount, AccountError> {
        let (tokens, last_refresh) = {
            let _auth_lock = FileLock::acquire(self.auth_json_path())?;
            let auth_val = self
                .read_active_auth()?
                .ok_or_else(|| AccountError::AuthJsonInvalid("auth.json not found".into()))?;

            if auth_val.get("auth_mode").and_then(|value| value.as_str()) != Some("chatgpt") {
                return Err(AccountError::AuthJsonInvalid(
                    "auth.json is not using the official `chatgpt` auth mode".into(),
                ));
            }

            let tokens_obj = auth_val
                .get("tokens")
                .ok_or_else(|| AccountError::AuthJsonInvalid("tokens field missing".into()))?;

            let tokens: AccountTokens = serde_json::from_value(tokens_obj.clone())
                .map_err(|e| AccountError::AuthJsonInvalid(format!("invalid tokens: {e}")))?;

            let last_refresh = auth_val
                .get("last_refresh")
                .and_then(|v| v.as_str())
                .map(String::from);
            (tokens, last_refresh)
        };

        self.import_or_update_account(tokens, last_refresh, custom_name)
    }

    /// Import or update an account given its tokens
    pub fn import_or_update_account(
        &self,
        tokens: AccountTokens,
        last_refresh: Option<String>,
        name_hint: Option<String>,
    ) -> Result<ManagedAccount, AccountError> {
        let profile = validate_import_tokens(&tokens)?;

        let now = now_secs();
        let email = profile.email.clone();
        let plan_type = profile.plan_type.clone();
        let user_id = profile.user_id.clone();
        let account_id = tokens
            .account_id
            .clone()
            .or_else(|| profile.chatgpt_account_id.clone());

        let (mut file, _lock) = self.load_file_locked()?;
        let email_index = file.accounts.iter().position(
            |acc| matches!((&acc.email, &email), (Some(a), Some(e)) if a.eq_ignore_ascii_case(e)),
        );
        let account_id_index = file
            .accounts
            .iter()
            .position(|acc| matches!((&acc.account_id, &account_id), (Some(a), Some(e)) if a == e));
        if let (Some(by_email), Some(by_account_id)) = (email_index, account_id_index)
            && by_email != by_account_id
        {
            return Err(AccountError::InvalidToken(
                "the supplied token identity matches two different managed accounts".into(),
            ));
        }
        let existing_index = email_index.or(account_id_index);
        let credential_state = tokens.credential_state();

        let account = if let Some(idx) = existing_index {
            let acc = &mut file.accounts[idx];
            let tokens_changed = acc.tokens != tokens;
            if let Some(n) = name_hint {
                acc.name = n;
            }
            acc.tokens = tokens;
            if last_refresh.is_some() {
                acc.last_refresh = last_refresh;
            }
            if email.is_some() {
                acc.email = email;
            }
            if plan_type.is_some() {
                acc.plan_type = plan_type;
            }
            if user_id.is_some() {
                acc.user_id = user_id;
            }
            if account_id.is_some() {
                acc.account_id = account_id;
            }
            acc.credential_state = credential_state.clone();
            acc.credential_issue = None;
            acc.usage_issue = None;
            if tokens_changed {
                // A quota snapshot belongs to the previous credential
                // generation. Keeping it while clearing the error would make
                // the panel call old data "fresh" after re-capture.
                acc.usage = None;
            }
            acc.updated_at = now;
            acc.clone()
        } else {
            let id = uuid::Uuid::new_v4().to_string();
            let name = name_hint
                .unwrap_or_else(|| email.as_deref().unwrap_or("ChatGPT Account").to_string());

            let new_acc = ManagedAccount {
                id,
                name,
                email,
                plan_type,
                user_id,
                account_id,
                tokens,
                last_refresh,
                created_at: now,
                updated_at: now,
                usage: None,
                credential_state,
                credential_issue: None,
                usage_issue: None,
            };
            file.accounts.push(new_acc.clone());
            new_acc
        };

        self.save_file(&file)?;
        Ok(account)
    }

    pub fn delete_account(&self, id: &str) -> Result<bool, AccountError> {
        let (mut file, _lock) = self.load_file_locked()?;
        let before_len = file.accounts.len();
        file.accounts.retain(|acc| acc.id != id);
        if file.accounts.len() == before_len {
            return Ok(false);
        }
        self.save_file(&file)?;
        // The store no longer references this id, so its keyring entry must go
        // too; otherwise a removed account keeps a live refresh token around.
        // Silently printing a warning here meant the caller reported a clean
        // deletion while a usable credential was still on the machine, so this
        // now surfaces as an error the operator can act on.
        self.credentials
            .delete(&account_credential_reference(id))
            .map_err(|error| {
                AccountError::Credential(CredentialStoreError::Backend(format!(
                    "removed account `{id}` from the store but could not delete its stored                      credentials; a live token may remain: {error}"
                )))
            })?;
        Ok(true)
    }

    pub fn rename_account(&self, id: &str, new_name: &str) -> Result<(), AccountError> {
        let (mut file, _lock) = self.load_file_locked()?;
        let acc = file
            .accounts
            .iter_mut()
            .find(|a| a.id == id)
            .ok_or_else(|| AccountError::NotFound(id.to_string()))?;
        acc.name = new_name.trim().to_string();
        acc.updated_at = now_secs();
        self.save_file(&file)?;
        Ok(())
    }

    // ---------------- Account Switching ---------------- //

    /// Switch active auth.json to the chosen managed account
    pub fn switch_to_account(&self, id: &str) -> Result<ManagedAccount, AccountError> {
        let (file, _accounts_lock) = self.load_file_locked()?;
        let acc_idx = file
            .accounts
            .iter()
            .position(|a| a.id == id)
            .ok_or_else(|| AccountError::NotFound(id.to_string()))?;

        let account = file.accounts[acc_idx].clone();

        // Last line of defence. Writing `"tokens": {}` into auth.json logs the
        // user out and discards the only copy of their session, so a switch that
        // cannot actually resolve credentials must fail instead of proceeding.
        if !account.tokens.has_usable_credentials() {
            return Err(AccountError::MissingCredentials(id.to_owned()));
        }
        if account.credential_state == CredentialState::NeedsReauth {
            return Err(AccountError::RequiresReauthentication(id.to_owned()));
        }

        let auth_path = self.auth_json_path();
        let _auth_lock = FileLock::acquire(&auth_path)?;

        // Read existing auth.json if any to preserve other untouched fields (like OPENAI_API_KEY if present)
        let mut auth_doc: serde_json::Map<String, serde_json::Value> =
            match self.read_active_auth()? {
                Some(serde_json::Value::Object(map)) => map,
                _ => serde_json::Map::new(),
            };

        // Update auth_mode to chatgpt
        auth_doc.insert(
            "auth_mode".into(),
            serde_json::Value::String("chatgpt".into()),
        );

        // Serialize tokens
        let tokens_val = serde_json::to_value(&account.tokens)?;
        auth_doc.insert("tokens".into(), tokens_val);

        if let Some(lr) = &account.last_refresh {
            auth_doc.insert("last_refresh".into(), serde_json::Value::String(lr.clone()));
        } else {
            auth_doc.insert(
                "last_refresh".into(),
                serde_json::Value::String(chrono_iso_now()),
            );
        }

        // Backup existing auth.json before writing. A backup contains a live
        // refresh token, so old copies are pruned to a small bounded set and each
        // one is written through an atomic, 0600 temp file.
        if auth_path.exists() {
            self.write_auth_backup(&auth_path)?;
        } else if let Some(parent) = auth_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut bytes = serde_json::to_vec_pretty(&auth_doc)?;
        bytes.push(b'\n');
        // `auth.json` holds live OAuth tokens; it must never be group/world
        // readable, not even for the instant between create and chmod.
        codex_mp_core::write_private_atomic(&auth_path, &bytes)?;

        Ok(account)
    }

    /// Copy the current `auth.json` aside before a switch, keeping only the most
    /// recent [`MAX_AUTH_BACKUPS`] copies.
    ///
    /// Every copy holds a live refresh token, so they must be bounded, written
    /// privately, and named uniquely enough that two switches in the same second
    /// cannot overwrite each other's evidence.
    fn write_auth_backup(&self, auth_path: &Path) -> Result<(), AccountError> {
        let bytes = fs::read(auth_path)?;

        let backup_path = auth_path.with_extension(format!(
            "bak-switch-{}-{}",
            now_secs(),
            uuid::Uuid::new_v4()
        ));
        // A backup is a verbatim copy of `auth.json`, i.e. a full set of live
        // tokens, so it is created privately too.
        codex_mp_core::write_private_atomic(&backup_path, &bytes)?;

        self.prune_auth_backups(auth_path)?;
        Ok(())
    }

    fn prune_auth_backups(&self, auth_path: &Path) -> Result<(), AccountError> {
        let Some(parent) = auth_path.parent() else {
            return Ok(());
        };
        let Some(stem) = auth_path.file_stem().and_then(|s| s.to_str()) else {
            return Ok(());
        };
        let prefix = format!("{stem}.bak-switch-");

        let mut backups: Vec<PathBuf> = fs::read_dir(parent)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(&prefix))
            })
            .collect();

        if backups.len() <= MAX_AUTH_BACKUPS {
            return Ok(());
        }
        // Names end in a second-resolution timestamp, so a lexical sort is
        // chronological for every backup this code writes.
        backups.sort();
        let excess = backups.len() - MAX_AUTH_BACKUPS;
        for stale in backups.into_iter().take(excess) {
            if let Err(error) = fs::remove_file(&stale) {
                eprintln!(
                    "codex-mp: could not prune stale auth backup {}: {error}",
                    stale.display()
                );
            }
        }
        Ok(())
    }

    // ---------------- Refresh Token & Usage Query ---------------- //

    /// Refresh token if access token is missing or expired, updating stored account
    ///
    /// Takes `&Arc<Self>` so the keyring-backed persistence steps can be moved to
    /// a blocking thread. Calling `load_file`/`save_file` directly here panicked
    /// under a tokio runtime on Linux, where the keyring backend (`zbus`) reaches
    /// a synchronous API through `Runtime::block_on`.
    pub async fn refresh_account_token(
        self: &Arc<Self>,
        account_id: &str,
    ) -> Result<ManagedAccount, AccountError> {
        self.refresh_account_token_inner(account_id, None).await
    }

    async fn refresh_account_token_if_unchanged(
        self: &Arc<Self>,
        account_id: &str,
        observed_tokens: &AccountTokens,
    ) -> Result<ManagedAccount, AccountError> {
        self.refresh_account_token_inner(account_id, Some(observed_tokens.clone()))
            .await
    }

    async fn refresh_account_token_inner(
        self: &Arc<Self>,
        account_id: &str,
        observed_tokens: Option<AccountTokens>,
    ) -> Result<ManagedAccount, AccountError> {
        let refresh_lock = self.refresh_lock_for(account_id);
        let _in_process_guard = refresh_lock.lock().await;

        // Hold the accounts-store lock over the bounded network round trip so
        // another OmniBridge process cannot rotate the same refresh token at
        // the same time. The in-process lock above coalesces local callers.
        let store_path = self.store_path.clone();
        let _disk_guard = tokio::task::spawn_blocking(move || FileLock::acquire(store_path))
            .await
            .map_err(|error| AccountError::RefreshFailed(error.to_string()))??;

        let account_id_owned = account_id.to_owned();
        let this = self.clone();
        let mut file = tokio::task::spawn_blocking(move || this.load_file())
            .await
            .map_err(|error| AccountError::RefreshFailed(error.to_string()))??;
        let acc_idx = file
            .accounts
            .iter()
            .position(|a| a.id == account_id_owned)
            .ok_or_else(|| AccountError::NotFound(account_id_owned.clone()))?;

        // A caller that observed a 401 may have raced another refresh. If the
        // account has already changed since that observation, use the rotated
        // bundle instead of consuming the new refresh token a second time.
        if let Some(observed_tokens) = observed_tokens
            && file.accounts[acc_idx].tokens != observed_tokens
            && file.accounts[acc_idx].tokens.access_token.is_some()
        {
            return Ok(file.accounts[acc_idx].clone());
        }

        // Re-check after taking the refresh lock: a user may have switched to
        // this account while the usage request was in flight. Codex remains
        // the sole refresh owner for the active auth.json session.
        let active_check = {
            let this = self.clone();
            tokio::task::spawn_blocking(move || this.check_active_status())
                .await
                .map_err(|error| AccountError::RefreshFailed(error.to_string()))??
        };
        if active_check.is_logged_in
            && active_check.matched_account_id.as_deref() == Some(account_id)
        {
            return Err(AccountError::RequiresReauthentication(account_id_owned));
        }

        let refresh_token = file.accounts[acc_idx]
            .tokens
            .refresh_token
            .as_deref()
            .map(str::to_owned)
            .ok_or_else(|| AccountError::RequiresReauthentication(account_id_owned.clone()))?;

        let form_body = form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "refresh_token")
            .append_pair("client_id", OPENAI_CLIENT_ID)
            .append_pair("refresh_token", &refresh_token)
            .finish();

        let resp = self
            .http_client
            .post(OPENAI_OAUTH_TOKEN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("User-Agent", DEFAULT_USER_AGENT)
            .body(form_body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err_text = read_bounded_error_text(resp).await;
            let (code, message) = oauth_error_details(&err_text);
            if is_reauthentication_code(code.as_deref(), status) {
                return Err(AccountError::ReauthenticationRequired {
                    account_id: account_id_owned,
                    code: code.unwrap_or_else(|| "refresh_unauthorized".into()),
                    message,
                });
            }
            return Err(AccountError::RefreshFailed(format!(
                "HTTP {status}: {message}"
            )));
        }

        let token_resp = read_bounded_account_json(resp, "the token endpoint").await?;
        let new_access_token = token_resp
            .get("access_token")
            .and_then(|v| v.as_str())
            .filter(|value| !value.trim().is_empty())
            .map(String::from);
        let new_refresh_token = token_resp
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .filter(|value| !value.trim().is_empty())
            .map(String::from);
        let new_id_token = token_resp
            .get("id_token")
            .and_then(|v| v.as_str())
            .filter(|value| !value.trim().is_empty())
            .map(String::from);
        let response_account_id = token_resp
            .get("account_id")
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
            .map(String::from);

        let new_access_token = new_access_token.ok_or_else(|| {
            AccountError::RefreshFailed(
                "the token endpoint returned 200 without a usable access_token".into(),
            )
        })?;

        let acc = &mut file.accounts[acc_idx];
        acc.tokens.access_token = Some(new_access_token);
        if let Some(rt) = new_refresh_token {
            acc.tokens.refresh_token = Some(rt);
        }
        if let Some(it) = new_id_token {
            let profile = parse_id_token_claims(&it).map_err(|error| {
                AccountError::RefreshFailed(format!("the refreshed id_token is invalid: {error}"))
            })?;
            if let (Some(old), Some(new)) = (&acc.email, &profile.email)
                && !old.eq_ignore_ascii_case(new)
            {
                return Err(AccountError::AccountIdentityMismatch(format!(
                    "email changed from `{old}` to `{new}`"
                )));
            }
            if let Some(new_account_id) = profile.chatgpt_account_id.as_deref() {
                if let Some(old) = acc.account_id.as_deref()
                    && old != new_account_id
                {
                    return Err(AccountError::AccountIdentityMismatch(format!(
                        "account id changed from `{old}` to `{new_account_id}`"
                    )));
                }
                acc.account_id = Some(new_account_id.to_owned());
                acc.tokens.account_id = Some(new_account_id.to_owned());
            }
            if let Some(email) = profile.email {
                acc.email = Some(email);
            }
            if profile.plan_type.is_some() {
                acc.plan_type = profile.plan_type;
            }
            acc.tokens.id_token = Some(it);
        }
        if let Some(new_account_id) = response_account_id {
            if let Some(old) = acc.account_id.as_deref()
                && old != new_account_id
            {
                return Err(AccountError::AccountIdentityMismatch(format!(
                    "account id changed from `{old}` to `{new_account_id}`"
                )));
            }
            acc.account_id = Some(new_account_id.clone());
            acc.tokens.account_id = Some(new_account_id);
        }
        acc.credential_state = CredentialState::Ready;
        acc.credential_issue = None;
        acc.usage_issue = None;
        acc.last_refresh = Some(chrono_iso_now());
        acc.updated_at = now_secs();

        let updated_acc = acc.clone();
        let this = self.clone();
        let file_to_save = file.clone();
        tokio::task::spawn_blocking(move || this.save_file(&file_to_save))
            .await
            .map_err(|error| AccountError::RefreshFailed(error.to_string()))??;

        Ok(updated_acc)
    }

    /// Fetch usage / rate limits for a given account. The active account uses
    /// the live token from `auth.json`; inactive accounts may refresh their own
    /// managed token, but never the active Codex-owned session.
    pub async fn fetch_usage(
        self: &Arc<Self>,
        account_id: &str,
    ) -> Result<AccountUsageSnapshot, AccountError> {
        let result = self.fetch_usage_inner(account_id).await;
        if let Err(error) = &result
            && let Err(record_error) = self.record_usage_failure_offloaded(account_id, error).await
        {
            eprintln!(
                "codex-mp: could not persist usage failure for `{account_id}`: {record_error}"
            );
        }
        result
    }

    async fn fetch_usage_inner(
        self: &Arc<Self>,
        account_id: &str,
    ) -> Result<AccountUsageSnapshot, AccountError> {
        let (account, active_live_auth) = self.load_account_for_usage_offloaded(account_id).await?;

        match self.do_fetch_usage(&account).await {
            Ok(snapshot) => {
                self.save_usage_snapshot_offloaded(account_id, snapshot.clone())
                    .await?;
                Ok(snapshot)
            }
            Err(AccountError::UsageCheckFailed { status: 401, .. }) => {
                // Codex may have rotated its live token between the first
                // read and the 401. Re-read once, but never refresh it here.
                let (latest, latest_is_active) =
                    self.load_account_for_usage_offloaded(account_id).await?;
                if latest_is_active && (active_live_auth || latest.tokens != account.tokens) {
                    return self.retry_usage_once(account_id, latest).await;
                }
                if active_live_auth || latest_is_active {
                    return Err(AccountError::ReauthenticationRequired {
                        account_id: account_id.to_owned(),
                        code: "live_session_unauthorized".into(),
                        message: "the live Codex session was rejected by the usage endpoint".into(),
                    });
                }

                // Another request may have completed a refresh while this one
                // was waiting. Reuse its rotated token rather than refreshing
                // a second time.
                if latest.tokens != account.tokens {
                    return self.retry_usage_once(account_id, latest).await;
                }

                let refreshed = self
                    .refresh_account_token_if_unchanged(account_id, &account.tokens)
                    .await?;
                self.retry_usage_once(account_id, refreshed).await
            }
            Err(e) => Err(e),
        }
    }

    async fn retry_usage_once(
        self: &Arc<Self>,
        account_id: &str,
        account: ManagedAccount,
    ) -> Result<AccountUsageSnapshot, AccountError> {
        match self.do_fetch_usage(&account).await {
            Ok(snapshot) => {
                self.save_usage_snapshot_offloaded(account_id, snapshot.clone())
                    .await?;
                Ok(snapshot)
            }
            Err(AccountError::UsageCheckFailed { status: 401, .. }) => {
                Err(AccountError::ReauthenticationRequired {
                    account_id: account_id.to_owned(),
                    code: "usage_unauthorized".into(),
                    message: "the usage endpoint rejected the refreshed credential".into(),
                })
            }
            Err(error) => Err(error),
        }
    }

    fn load_account_for_usage(
        &self,
        account_id: &str,
    ) -> Result<(ManagedAccount, bool), AccountError> {
        let file = self.load_file()?;
        let account = file
            .accounts
            .into_iter()
            .find(|account| account.id == account_id)
            .ok_or_else(|| AccountError::NotFound(account_id.to_owned()))?;
        let active = self.check_active_status()?;
        if active.is_logged_in
            && active.auth_mode.as_deref() == Some("chatgpt")
            && active.matched_account_id.as_deref() == Some(account_id)
        {
            let _auth_lock = FileLock::acquire(self.auth_json_path())?;
            let auth = self
                .read_active_auth()?
                .ok_or_else(|| AccountError::AuthJsonInvalid("auth.json disappeared".into()))?;
            let tokens_value = auth
                .get("tokens")
                .cloned()
                .ok_or_else(|| AccountError::AuthJsonInvalid("tokens field missing".into()))?;
            let live_tokens: AccountTokens =
                serde_json::from_value(tokens_value).map_err(|error| {
                    AccountError::AuthJsonInvalid(format!("invalid tokens: {error}"))
                })?;
            if live_tokens.has_usable_credentials() {
                let mut live_account = account;
                if live_tokens.account_id.is_some() {
                    live_account.account_id = live_tokens.account_id.clone();
                }
                live_account.tokens = live_tokens;
                return Ok((live_account, true));
            }
        }
        Ok((account, false))
    }

    async fn load_account_for_usage_offloaded(
        self: &Arc<Self>,
        account_id: &str,
    ) -> Result<(ManagedAccount, bool), AccountError> {
        let this = self.clone();
        let account_id = account_id.to_owned();
        tokio::task::spawn_blocking(move || this.load_account_for_usage(&account_id))
            .await
            .map_err(|error| AccountError::UsageCheckFailed {
                status: 500,
                message: error.to_string(),
            })?
    }

    /// `save_usage_snapshot` is keyring-backed, so it must not run on an async
    /// worker thread (see `refresh_account_token`).
    async fn save_usage_snapshot_offloaded(
        self: &Arc<Self>,
        account_id: &str,
        snapshot: AccountUsageSnapshot,
    ) -> Result<(), AccountError> {
        let this = self.clone();
        let account_id_owned = account_id.to_owned();
        tokio::task::spawn_blocking(move || this.save_usage_snapshot(&account_id_owned, snapshot))
            .await
            .map_err(|error| AccountError::UsageCheckFailed {
                status: 500,
                message: error.to_string(),
            })?
    }

    async fn record_usage_failure_offloaded(
        self: &Arc<Self>,
        account_id: &str,
        error: &AccountError,
    ) -> Result<(), AccountError> {
        let issue = AccountIssue {
            code: error.public_code().to_owned(),
            message: error.public_message(),
            occurred_at: now_secs(),
        };
        let needs_reauth = issue.code == "reauth_required";
        let this = self.clone();
        let account_id = account_id.to_owned();
        tokio::task::spawn_blocking(move || {
            this.record_usage_failure(&account_id, issue, needs_reauth)
        })
        .await
        .map_err(|error| AccountError::UsageCheckFailed {
            status: 500,
            message: error.to_string(),
        })?
    }

    fn record_usage_failure(
        &self,
        account_id: &str,
        issue: AccountIssue,
        needs_reauth: bool,
    ) -> Result<(), AccountError> {
        let (mut file, _lock) = self.load_file_locked()?;
        if let Some(account) = file
            .accounts
            .iter_mut()
            .find(|account| account.id == account_id)
        {
            // A slower failed request must not overwrite a successful usage
            // snapshot or a newer credential rotation that completed after it
            // started.
            if account.updated_at > issue.occurred_at {
                return Ok(());
            }
            account.usage_issue = Some(issue.clone());
            if needs_reauth {
                account.credential_state = CredentialState::NeedsReauth;
                account.credential_issue = Some(issue);
            }
            account.updated_at = now_secs();
            self.save_file(&file)?;
        }
        Ok(())
    }

    async fn do_fetch_usage(
        &self,
        account: &ManagedAccount,
    ) -> Result<AccountUsageSnapshot, AccountError> {
        let access_token = account.tokens.access_token.as_deref().ok_or_else(|| {
            AccountError::UsageCheckFailed {
                status: 401,
                message: "No access_token available".into(),
            }
        })?;

        let mut req = self
            .http_client
            .get(OPENAI_WHAM_USAGE_URL)
            .header("Authorization", format!("Bearer {access_token}"))
            .header("User-Agent", "Mozilla/5.0");

        if let Some(aid) = &account.account_id {
            req = req.header("Chatgpt-Account-Id", aid);
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let msg = read_bounded_error_text(resp).await;
            return Err(AccountError::UsageCheckFailed {
                status,
                message: msg,
            });
        }

        let body = read_bounded_account_json(resp, "the usage endpoint").await?;
        let snapshot = parse_wham_usage_response(&body);
        Ok(snapshot)
    }

    fn save_usage_snapshot(
        &self,
        account_id: &str,
        snapshot: AccountUsageSnapshot,
    ) -> Result<(), AccountError> {
        let (mut file, _lock) = self.load_file_locked()?;
        if let Some(acc) = file.accounts.iter_mut().find(|a| a.id == account_id) {
            acc.usage = Some(snapshot);
            acc.usage_issue = None;
            acc.updated_at = now_secs();
            self.save_file(&file)?;
        }
        Ok(())
    }

    // ---------------- Restarting Codex ---------------- //

    /// Restart Codex processes (Desktop app-server and CLI background processes)
    pub fn restart_codex_processes(&self) -> Result<RestartCodexReport, AccountError> {
        let mut killed_pids = Vec::new();

        // 1. Terminate running codex app-server processes. Only report PIDs
        //    that are actually gone, so the UI cannot claim a clean restart
        //    while a process is still holding the old credentials.
        let running_pids = find_codex_running_pids();
        for pid in &running_pids {
            if terminate_pid(*pid) {
                killed_pids.push(*pid);
            } else {
                eprintln!("codex-mp: could not terminate Codex app-server pid {pid}");
            }
        }

        // 2. Remove stale app-server socket if present
        let sock_path = self
            .codex_home
            .join("app-server-control/app-server-control.sock");
        if sock_path.exists() {
            let _ = fs::remove_file(&sock_path);
        }

        Ok(RestartCodexReport {
            terminated_pids: killed_pids,
            message: "Codex background app-server terminated. Desktop/CLI will reload credentials on next invocation or window focus.".into(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestartCodexReport {
    pub terminated_pids: Vec<u32>,
    pub message: String,
}

// ---------------- Helpers ---------------- //

pub fn default_accounts_path() -> PathBuf {
    ProjectDirs::from("dev", "codex-multiprovider", "Codex MultiProvider")
        .map(|dirs| dirs.config_dir().join("accounts.json"))
        .unwrap_or_else(|| PathBuf::from("accounts.json"))
}

pub fn default_codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| BaseDirs::new().map(|dirs| dirs.home_dir().join(".codex")))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Current UTC time as an RFC 3339 timestamp, e.g. `2026-09-17T12:34:56Z`.
///
/// `auth.json` stores `last_refresh` as a timestamp string that Codex parses;
/// writing bare Unix seconds there corrupted its refresh bookkeeping.
fn chrono_iso_now() -> String {
    let secs = now_secs();
    let days = secs / 86_400;
    let seconds_of_day = secs % 86_400;
    let (hour, minute, second) = (
        seconds_of_day / 3_600,
        (seconds_of_day % 3_600) / 60,
        seconds_of_day % 60,
    );
    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Convert a count of days since the Unix epoch into a civil (year, month, day).
/// Howard Hinnant's `civil_from_days` algorithm; valid for the whole range we care
/// about and avoids pulling in a date crate.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

pub fn parse_id_token_claims(id_token: &str) -> Result<ProfileInfo, AccountError> {
    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() < 2 {
        return Err(AccountError::InvalidToken("id_token parts < 2".into()));
    }
    let payload_b64 = parts[1];
    let decoded = URL_SAFE_NO_PAD
        .decode(payload_b64.as_bytes())
        .or_else(|_| {
            let padded = match payload_b64.len() % 4 {
                2 => format!("{payload_b64}=="),
                3 => format!("{payload_b64}="),
                _ => payload_b64.to_string(),
            };
            URL_SAFE_NO_PAD.decode(padded.as_bytes())
        })
        .map_err(|e| AccountError::InvalidToken(format!("base64 decode error: {e}")))?;

    let claims: serde_json::Value = serde_json::from_slice(&decoded)?;

    let email = claims
        .get("email")
        .and_then(|v| v.as_str())
        .map(String::from);
    let name = claims
        .get("name")
        .and_then(|v| v.as_str())
        .map(String::from);
    let sub = claims.get("sub").and_then(|v| v.as_str()).map(String::from);

    let auth_claim = claims.get("https://api.openai.com/auth");
    let plan_type = auth_claim
        .and_then(|a| a.get("chatgpt_plan_type"))
        .and_then(|v| v.as_str())
        .map(String::from);
    let user_id = auth_claim
        .and_then(|a| a.get("chatgpt_user_id").or_else(|| a.get("user_id")))
        .and_then(|v| v.as_str())
        .map(String::from);
    let chatgpt_account_id = auth_claim
        .and_then(|a| a.get("chatgpt_account_id"))
        .and_then(|v| v.as_str())
        .map(String::from);

    Ok(ProfileInfo {
        email,
        name,
        sub,
        plan_type,
        user_id,
        chatgpt_account_id,
    })
}

/// Validate the minimum identity and credential material required for a
/// managed official account. JWT signatures are still validated by OpenAI when
/// the token is used; this local check only prevents obviously incomplete or
/// malformed imports from becoming indistinguishable from a healthy account.
fn validate_import_tokens(tokens: &AccountTokens) -> Result<ProfileInfo, AccountError> {
    if !tokens.has_usable_credentials() {
        return Err(AccountError::InvalidToken(
            "an account import must contain an access_token or refresh_token".into(),
        ));
    }

    let profile = match tokens.id_token.as_deref() {
        Some(id_token) => parse_id_token_claims(id_token)?,
        None => ProfileInfo::default(),
    };
    if let (Some(explicit), Some(claimed)) = (
        tokens.account_id.as_deref(),
        profile.chatgpt_account_id.as_deref(),
    ) && explicit != claimed
    {
        return Err(AccountError::InvalidToken(
            "the supplied account_id does not match the id_token identity".into(),
        ));
    }
    let has_identity = tokens.account_id.is_some()
        || profile.email.is_some()
        || profile.chatgpt_account_id.is_some();
    if !has_identity {
        return Err(AccountError::InvalidToken(
            "an account import must contain an account_id or a parseable id_token identity".into(),
        ));
    }
    Ok(profile)
}

fn parse_usage_window(value: &serde_json::Value) -> Option<UsageWindow> {
    let used = value.get("used_percent").and_then(|value| value.as_u64())?;
    let window_secs = value
        .get("limit_window_seconds")
        .and_then(|value| value.as_u64())?;
    Some(UsageWindow {
        used_percent: used.min(100) as u32,
        limit_window_seconds: window_secs,
        reset_after_seconds: value
            .get("reset_after_seconds")
            .and_then(|value| value.as_u64())
            .unwrap_or(0),
        reset_at: value
            .get("reset_at")
            .and_then(|value| value.as_u64())
            .unwrap_or(0),
    })
}

pub fn parse_wham_usage_response(val: &serde_json::Value) -> AccountUsageSnapshot {
    let plan_type = val
        .get("plan_type")
        .and_then(|v| v.as_str())
        .map(String::from);

    let mut primary_5h: Option<UsageWindow> = None;
    let mut secondary_weekly: Option<UsageWindow> = None;

    if let Some(rate_limit) = val.get("rate_limit") {
        if let Some(pw) = rate_limit.get("primary_window")
            && let Some(window) = parse_usage_window(pw)
        {
            if window.limit_window_seconds <= 18000 {
                primary_5h = Some(window);
            } else {
                secondary_weekly = Some(window);
            }
        }
        if let Some(sw) = rate_limit.get("secondary_window").filter(|v| !v.is_null()) {
            secondary_weekly = parse_usage_window(sw);
        }
    }

    let mut reserve: Option<ReserveLimitSummary> = None;
    if let Some(add_arr) = val.get("additional_rate_limits").and_then(|v| v.as_array()) {
        for item in add_arr {
            let name = item
                .get("limit_name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if name.contains("reserve") {
                let rl = item.get("rate_limit");
                let allowed = rl
                    .and_then(|r| r.get("allowed"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let reached = rl
                    .and_then(|r| r.get("limit_reached"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let pw = rl.and_then(|r| r.get("primary_window"));
                let used = pw
                    .and_then(|p| p.get("used_percent"))
                    .and_then(|v| v.as_u64())
                    .map(|v| v.min(100) as u32);
                let reset_after = pw
                    .and_then(|p| p.get("reset_after_seconds"))
                    .and_then(|v| v.as_u64());
                let reset_at = pw.and_then(|p| p.get("reset_at")).and_then(|v| v.as_u64());

                reserve = Some(ReserveLimitSummary {
                    limit_name: name.to_string(),
                    allowed,
                    limit_reached: reached,
                    used_percent: used,
                    reset_after_seconds: reset_after,
                    reset_at,
                });
                break;
            }
        }
    }

    AccountUsageSnapshot {
        updated_at: now_secs(),
        plan_type,
        primary_5h,
        secondary_weekly,
        reserve,
    }
}

/// Image names that identify a real Codex runtime. Matching on a `codex`
/// substring instead would also select `codex-mp.exe` and `codex-mp-panel.exe`,
/// i.e. the very process asking for the restart.
#[cfg(target_os = "windows")]
const CODEX_IMAGE_NAMES: [&str; 3] = ["codex.exe", "chatgpt.exe", "codex-code-mode-host.exe"];

/// True when a Windows image name refers to a Codex runtime.
#[cfg(target_os = "windows")]
fn is_codex_image(image: &str) -> bool {
    let image = image.trim().to_ascii_lowercase();
    CODEX_IMAGE_NAMES.contains(&image.as_str())
}

/// Find running Codex PIDs.
///
/// Every platform narrows on the `app-server` argument: a bare Codex CLI session
/// or a `codex-mp` process must never be killed by the account switcher.
pub fn find_codex_running_pids() -> Vec<u32> {
    #[cfg(target_os = "linux")]
    {
        let mut pids = Vec::new();
        let Ok(entries) = fs::read_dir("/proc") else {
            return pids;
        };
        for entry in entries.flatten() {
            let Ok(pid) = entry
                .file_name()
                .to_str()
                .unwrap_or_default()
                .parse::<u32>()
            else {
                continue;
            };
            let cmdline_path = format!("/proc/{pid}/cmdline");
            if let Ok(cmdline) = fs::read(&cmdline_path) {
                // `/proc/<pid>/cmdline` is NUL-separated; split it so matching is
                // done per argument rather than on a joined string.
                let args: Vec<String> = cmdline
                    .split(|byte| *byte == 0)
                    .filter(|arg| !arg.is_empty())
                    .map(|arg| String::from_utf8_lossy(arg).into_owned())
                    .collect();
                if is_codex_app_server_process(&args, pid) {
                    pids.push(pid);
                }
            }
        }
        pids.sort_unstable();
        pids.dedup();
        pids
    }

    #[cfg(target_os = "windows")]
    {
        let mut pids = Vec::new();
        let own_pid = std::process::id();
        let output = std::process::Command::new("tasklist.exe")
            .args(["/FO", "CSV", "/NH"])
            .output();
        if let Ok(out) = output {
            let stdout = String::from_utf8_lossy(&out.stdout);
            for line in stdout.lines() {
                let fields: Vec<&str> = line
                    .split(',')
                    .map(|s| s.trim().trim_matches('"'))
                    .collect();
                if fields.len() < 2 {
                    continue;
                }
                // Exact image-name match, plus an app-server command-line check
                // via wmic-equivalent listing below, keeps this in line with the
                // Linux/macOS semantics.
                if !is_codex_image(fields[0]) {
                    continue;
                }
                let Ok(pid) = fields[1].parse::<u32>() else {
                    continue;
                };
                // Never select this process or the parent that asked for the
                // restart. The image-name check already excludes `codex-mp`, but
                // the parent is a separate guard for a caller whose image happens
                // to match.
                if pid == own_pid || Some(pid) == parent_process_id() {
                    continue;
                }
                if process_command_line_contains(pid, "app-server") {
                    pids.push(pid);
                }
            }
        }
        pids.sort_unstable();
        pids.dedup();
        pids
    }

    #[cfg(target_os = "macos")]
    {
        // `pgrep -f "codex.*app-server"` matched by regular expression over the
        // whole command line, so it also selected `codex-mp ... app-server ...`
        // (our own binary, and the parent that requested the restart) as well as
        // arguments that merely contained the word. Enumerate with `ps` instead
        // and apply the same argv-level predicate used on Linux.
        let mut pids = Vec::new();
        // `-ww` disables truncation; `-o pid=,command=` prints one line per
        // process with no header.
        let output = std::process::Command::new("ps")
            .args(["-ww", "-A", "-o", "pid=,command="])
            .output();
        if let Ok(out) = output {
            let stdout = String::from_utf8_lossy(&out.stdout);
            for line in stdout.lines() {
                let line = line.trim_start();
                let Some((pid_text, command)) = line.split_once(char::is_whitespace) else {
                    continue;
                };
                let Ok(pid) = pid_text.parse::<u32>() else {
                    continue;
                };
                // `ps` prints the command line space-joined; the arguments the
                // predicate cares about (`app-server`) contain no spaces, so a
                // whitespace split is sufficient and argv[0] stays first.
                let args: Vec<String> = command.split_whitespace().map(str::to_owned).collect();
                if is_codex_app_server_process(&args, pid) {
                    pids.push(pid);
                }
            }
        }
        pids.sort_unstable();
        pids.dedup();
        pids
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        Vec::new()
    }
}

/// Whether `/proc/<pid>/cmdline` arguments describe a real Codex app-server.
///
/// The previous check matched any command line *containing* `codex` and
/// `app-server` and not equal to the current PID. That wrongly selected:
///
/// - the **parent** process (the panel or `codex-mp` invocation that spawned the
///   app-server), so "switch account" killed its own caller;
/// - unrelated `codex-mp` commands that merely mention `app-server`, e.g.
///   `codex-mp desktop install --app-server-binary ...`.
///
/// A match now requires argv[0] to be a Codex runtime binary (not `codex-mp`),
/// with `app-server` present as its own argument, and never selects this process
/// or its parent.
///
/// Only the Linux and macOS arms parse `/proc` / `ps` output; Windows uses
/// `tasklist`, so this is dead code there and would fail `-D dead-code`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn is_codex_app_server_process(args: &[String], pid: u32) -> bool {
    let Some(program) = args.first() else {
        return false;
    };
    let program_name = std::path::Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program)
        .to_ascii_lowercase();
    // `codex-mp` is us, never the thing we restart. `codex-real` is the stock
    // binary the launcher renames to.
    let is_codex_runtime =
        program_name.starts_with("codex") && !program_name.starts_with("codex-mp");
    if !is_codex_runtime {
        return false;
    }
    if pid == std::process::id() || Some(pid) == parent_process_id() {
        return false;
    }
    // `app-server` must be a standalone argument, not a substring of a path we
    // were merely told about.
    args.iter()
        .skip(1)
        .any(|arg| arg == "app-server" || arg.ends_with("/app-server"))
}

/// This process's parent PID, when it can be determined.
///
/// Killing the parent would take down the caller that asked for the restart (the
/// panel, or the `codex-mp` invocation), so it is never a candidate.
#[cfg(unix)]
fn parent_process_id() -> Option<u32> {
    Some(unsafe { libc::getppid() } as u32)
}

#[cfg(not(unix))]
fn parent_process_id() -> Option<u32> {
    None
}

/// Best-effort Windows command-line lookup used to narrow the process match.
/// Returns `true` when the command line cannot be read, so an unreadable process
/// is still considered (matching the looser Linux behaviour).
#[cfg(target_os = "windows")]
fn process_command_line_contains(pid: u32, needle: &str) -> bool {
    let script = format!("(Get-CimInstance Win32_Process -Filter \"ProcessId={pid}\").CommandLine");
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output();
    match output {
        Ok(out) if out.status.success() => {
            let line = String::from_utf8_lossy(&out.stdout);
            line.trim().is_empty() || line.contains(needle)
        }
        _ => true,
    }
}

/// Terminate a process, returning whether it is believed to be gone.
///
/// SIGTERM alone is only a request: a process that ignores it was previously
/// reported as terminated while still running. Escalate to SIGKILL after a grace
/// period and report the real outcome.
pub fn terminate_pid(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe {
            if libc::kill(pid as i32, libc::SIGTERM) != 0 {
                // ESRCH means it is already gone, which counts as success.
                return std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            }
        }
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(50));
            let alive = unsafe { libc::kill(pid as i32, 0) } == 0;
            if !alive {
                return true;
            }
        }
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
        }
        std::thread::sleep(Duration::from_millis(50));
        unsafe { libc::kill(pid as i32, 0) != 0 }
    }
    #[cfg(windows)]
    {
        let output = std::process::Command::new("taskkill.exe")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output();
        match output {
            // 128 = "process not found", which means the goal is already met.
            Ok(out) => out.status.success() || out.status.code() == Some(128),
            Err(_) => false,
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// A credential store whose reads always fail, simulating a locked or
    /// unavailable keyring (exactly what happens on a headless Linux box).
    struct FailingReadStore;

    impl CredentialStore for FailingReadStore {
        fn get(&self, reference: &str) -> Result<SecretString, CredentialStoreError> {
            Err(CredentialStoreError::Backend(format!(
                "keyring unavailable for {reference}"
            )))
        }
        fn set(&self, _reference: &str, _value: &SecretString) -> Result<(), CredentialStoreError> {
            Ok(())
        }
        fn delete(&self, _reference: &str) -> Result<(), CredentialStoreError> {
            Ok(())
        }
    }

    /// A credential store that counts how many times each operation is called.
    #[derive(Default)]
    struct CountingStore {
        writes: std::sync::Mutex<Vec<String>>,
        inner: codex_mp_credentials::MemoryCredentialStore,
    }

    impl CredentialStore for CountingStore {
        fn get(&self, reference: &str) -> Result<SecretString, CredentialStoreError> {
            self.inner.get(reference)
        }
        fn set(&self, reference: &str, value: &SecretString) -> Result<(), CredentialStoreError> {
            self.writes.lock().unwrap().push(reference.to_owned());
            self.inner.set(reference, value)
        }
        fn delete(&self, reference: &str) -> Result<(), CredentialStoreError> {
            self.inner.delete(reference)
        }
    }

    /// Regression: `save_file` rewrote *every* account's tokens on every save, so
    /// a usage refresh for one account wrote all of them. With N accounts that is
    /// N keyring round-trips per refresh (N^2 per cycle), and a keyring problem on
    /// any single account failed an unrelated account's refresh.
    ///
    /// Saving an unchanged document must write nothing at all.
    /// Regression: a corrupted accounts store surfaced a bare parser message
    /// (`JSON error: expected value at line 1 column 15`) with no indication of
    /// *which* file was broken and no way to recover — account listing and import
    /// both failed, and the panel showed the message verbatim.
    ///
    /// The error must name the file and state the remedy.
    #[test]
    fn a_corrupt_accounts_store_names_the_file_and_the_remedy() {
        let directory = tempdir().unwrap();
        let store_path = directory.path().join("accounts.json");
        std::fs::write(&store_path, b"{\"accounts\": [broken").unwrap();

        let store =
            AccountManager::with_paths(store_path.clone(), directory.path().join("codex-home"));
        let error = store
            .load_file()
            .expect_err("a corrupt store must be reported")
            .to_string();

        assert!(
            error.contains(&store_path.display().to_string()),
            "the error must name the offending file, got: {error}"
        );
        assert!(
            error.contains("mv ") && error.contains(".bak"),
            "the error must state a concrete recovery step, got: {error}"
        );
        // The original parser detail is still useful, so it must survive.
        assert!(
            error.contains("expected value"),
            "the underlying parse detail must be kept, got: {error}"
        );
    }

    #[tokio::test]
    async fn purge_provider_data_tolerates_an_unreadable_registry() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("accounts.json");
        let codex_home = dir.path().join("codex");
        fs::create_dir_all(&codex_home).unwrap();

        let store = Arc::new(CountingStore::default());
        let manager =
            AccountManager::with_credential_store(&store_path, &codex_home, store.clone());

        let tokens = AccountTokens {
            id_token: None,
            access_token: Some("at-1".into()),
            refresh_token: Some("rt-1".into()),
            account_id: Some("acc-1".into()),
        };
        manager
            .import_or_update_account(tokens, None, Some("One".into()))
            .unwrap();

        let written_after_import = store.writes.lock().unwrap().len();
        assert!(
            written_after_import > 0,
            "importing an account must store its credentials"
        );

        // Re-saving the identical document must not touch the keyring again.
        let file = manager.load_file().unwrap();
        manager.save_file(&file).unwrap();
        assert_eq!(
            store.writes.lock().unwrap().len(),
            written_after_import,
            "saving an unchanged document must not rewrite any credential"
        );
    }

    /// Regression: the token and usage endpoints were read with
    /// `Response::json()`, which buffers the whole body with no limit. They are
    /// hard-coded official HTTPS endpoints, so this is defence in depth, but an
    /// intermediary returning something enormous would otherwise be absorbed
    /// wholesale.
    ///
    /// The server advertises a huge `content-length` and then sends **no body**,
    /// so the check is deterministic: the reader must reject on the advertised
    /// length alone, without trying to buffer anything.
    #[tokio::test]
    async fn the_account_response_bound_rejects_an_oversized_body() {
        use tokio::io::AsyncWriteExt;

        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                // Headers claim a body far past the cap; no body follows.
                let headers = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
                    MAX_ACCOUNT_RESPONSE_BYTES + 1
                );
                let _ = socket.write_all(headers.as_bytes()).await;
                let _ = socket.flush().await;
                // Hold the connection briefly so the client can read the headers.
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        });

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap();
        let response = client
            .get(format!("http://{addr}/"))
            .send()
            .await
            .expect("the header response must arrive");
        let result = read_bounded_account_json(response, "a test endpoint").await;
        assert!(
            result.is_err(),
            "an advertised length past the cap must be rejected before buffering"
        );
        server.abort();
    }

    /// Regression: when the keyring read failed, `load_file` left the account's
    /// tokens empty, and `switch_to_account` then wrote that empty token set into
    /// `auth.json` — logging the user out and destroying the only copy of their
    /// session. A switch that cannot resolve the tokens must fail instead.
    #[test]
    fn switch_refuses_to_write_empty_tokens_when_the_keyring_is_unavailable() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("accounts.json");
        let codex_home = dir.path().join("codex");
        fs::create_dir_all(&codex_home).unwrap();

        // Seed an account whose tokens live only in the (working) keyring.
        let good = Arc::new(codex_mp_credentials::MemoryCredentialStore::default());
        let seeder = AccountManager::with_credential_store(&store_path, &codex_home, good);
        let tokens = AccountTokens {
            id_token: Some(
                "dummy.eyJlbWFpbCI6ImtleXJpbmctdGVzdEBleGFtcGxlLmNvbSIsInN1YiI6IjcifQ.dummy".into(),
            ),
            access_token: Some("at-live".into()),
            refresh_token: Some("rt-live".into()),
            account_id: Some("acc-live".into()),
        };
        let account = seeder
            .import_or_update_account(tokens, None, Some("Live".into()))
            .unwrap();

        // Now the keyring becomes unavailable.
        let broken = AccountManager::with_credential_store(
            &store_path,
            &codex_home,
            Arc::new(FailingReadStore),
        );
        // The tokens are unreachable, so switching must fail.
        let result = broken.switch_to_account(&account.id);
        assert!(
            result.is_err(),
            "switch succeeded even though the account's credentials were unreadable"
        );
        assert!(
            !broken.auth_json_path().exists(),
            "a failed switch must not leave a rewritten auth.json behind"
        );
    }

    /// A store that refuses deletes, used to prove cleanup failures surface.
    struct FailingDeleteStore;

    impl CredentialStore for FailingDeleteStore {
        fn get(&self, _reference: &str) -> Result<SecretString, CredentialStoreError> {
            Ok(SecretString::from("{}"))
        }
        fn set(&self, _reference: &str, _value: &SecretString) -> Result<(), CredentialStoreError> {
            Ok(())
        }
        fn delete(&self, reference: &str) -> Result<(), CredentialStoreError> {
            Err(CredentialStoreError::Backend(format!(
                "cannot delete {reference}"
            )))
        }
    }

    /// Regression: a failure to delete the keyring entry was only printed, so
    /// `delete_account` reported success while a live refresh token stayed on the
    /// machine. That is a credential-retention problem, not a cosmetic warning.
    #[test]
    fn deleting_an_account_reports_a_failed_credential_cleanup() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("accounts.json");
        let codex_home = dir.path().join("codex");
        fs::create_dir_all(&codex_home).unwrap();

        let writer = AccountManager::with_credential_store(
            &store_path,
            &codex_home,
            Arc::new(codex_mp_credentials::MemoryCredentialStore::default()),
        );
        let account = writer
            .import_or_update_account(
                AccountTokens {
                    id_token: None,
                    access_token: Some("at".into()),
                    refresh_token: Some("rt".into()),
                    account_id: Some("acc".into()),
                },
                None,
                Some("ToDelete".into()),
            )
            .unwrap();

        let failing = AccountManager::with_credential_store(
            &store_path,
            &codex_home,
            Arc::new(FailingDeleteStore),
        );
        let result = failing.delete_account(&account.id);
        assert!(
            result.is_err(),
            "a credential that could not be deleted must be reported, not swallowed"
        );
    }

    /// Regression: the accounts store did the same unprotected
    /// `load -> mutate -> atomic_replace` cycle the registry used to do, so two
    /// concurrent imports could each read revision N and both replace it,
    /// silently dropping one account. Atomic rename prevents a torn file, not a
    /// lost update.
    /// Regression: the token-refresh and usage error paths read the whole body
    /// with `text()`, on endpoints that are official but still external. An error
    /// diagnostic only ever shows a short snippet, so an oversized body must be
    /// truncated rather than buffered.
    #[tokio::test]
    async fn an_oversized_error_body_is_truncated() {
        use tokio::io::AsyncWriteExt;

        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let body = "e".repeat(64 * 1024);
                let response = format!(
                    "HTTP/1.1 400 Bad Request\r\ncontent-type: text/plain\r\ncontent-length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        });

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap();
        let response = client
            .get(format!("http://{addr}/"))
            .send()
            .await
            .expect("the error response must arrive");
        let message = read_bounded_error_text(response).await;
        assert!(
            message.len() < 16 * 1024,
            "an oversized error body must be truncated, got {} bytes",
            message.len()
        );
        assert!(
            message.ends_with("(truncated)"),
            "the truncation must be visible in the message"
        );
        server.abort();
    }

    #[test]
    fn concurrent_account_imports_do_not_lose_an_account() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("accounts.json");
        let codex_home = dir.path().join("codex");
        fs::create_dir_all(&codex_home).unwrap();

        // Seed the file so every writer has something to read.
        let seeder = AccountManager::with_credential_store(
            &store_path,
            &codex_home,
            Arc::new(codex_mp_credentials::MemoryCredentialStore::default()),
        );
        seeder
            .import_or_update_account(
                AccountTokens {
                    id_token: None,
                    access_token: Some("seed".into()),
                    refresh_token: Some("seed".into()),
                    account_id: Some("seed".into()),
                },
                None,
                Some("Seed".into()),
            )
            .unwrap();

        let mut handles = Vec::new();
        for index in 0..8 {
            let store_path = store_path.clone();
            let codex_home = codex_home.clone();
            handles.push(std::thread::spawn(move || {
                let manager = AccountManager::with_credential_store(
                    &store_path,
                    &codex_home,
                    Arc::new(codex_mp_credentials::MemoryCredentialStore::default()),
                );
                manager
                    .import_or_update_account(
                        AccountTokens {
                            id_token: None,
                            access_token: Some(format!("at-{index}")),
                            refresh_token: Some(format!("rt-{index}")),
                            account_id: Some(format!("acc-{index}")),
                        },
                        None,
                        Some(format!("Account {index}")),
                    )
                    .unwrap();
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }

        let file: AccountsFile =
            serde_json::from_str(&fs::read_to_string(&store_path).unwrap()).unwrap();
        assert_eq!(
            file.accounts.len(),
            9,
            "an account was lost to a concurrent write (expected seed + 8)"
        );
    }

    /// Regression: the matcher used to select any process whose command line
    /// contained `codex` and `app-server`, which included the **parent** process
    /// (the panel or `codex-mp` invocation doing the restart) and unrelated
    /// `codex-mp` commands that merely mentioned `app-server`. "Switch account"
    /// could therefore kill its own caller.
    #[cfg(target_os = "linux")]
    #[test]
    fn only_real_codex_app_servers_are_selected_for_restart() {
        let own = std::process::id();
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // Must match: a real Codex runtime running the app-server.
        assert!(is_codex_app_server_process(
            &args(&[
                "/home/u/.codex/packages/standalone/releases/v/bin/codex-real",
                "app-server"
            ]),
            4242,
        ));
        assert!(is_codex_app_server_process(
            &args(&["/usr/bin/codex", "app-server"]),
            4243,
        ));

        // Must NOT match: our own binary, in any form.
        assert!(!is_codex_app_server_process(
            &args(&[
                "/home/u/.local/bin/codex-mp",
                "launch",
                "--app-server-binary",
                "/x"
            ]),
            4244,
        ));
        assert!(!is_codex_app_server_process(
            &args(&[
                "codex-mp",
                "desktop",
                "install",
                "--app-server-binary",
                "/x"
            ]),
            4245,
        ));

        // Must NOT match: this process.
        assert!(!is_codex_app_server_process(
            &args(&["codex", "app-server"]),
            own
        ));

        // Must NOT match: `app-server` only appears inside a path argument.
        assert!(!is_codex_app_server_process(
            &args(&["/usr/bin/codex", "--config", "/srv/app-server/config.toml"]),
            4246,
        ));

        // Must NOT match: a bare codex command with no app-server role.
        assert!(!is_codex_app_server_process(
            &args(&["/usr/bin/codex", "exec"]),
            4247
        ));
    }

    #[test]
    fn test_parse_wham_usage() {
        let sample = serde_json::json!({
            "plan_type": "plus",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 15,
                    "limit_window_seconds": 18000,
                    "reset_after_seconds": 12000,
                    "reset_at": 1789413197
                },
                "secondary_window": {
                    "used_percent": 50,
                    "limit_window_seconds": 604800,
                    "reset_after_seconds": 300000,
                    "reset_at": 1789805331
                }
            },
            "additional_rate_limits": [
                {
                    "limit_name": "gpt-reserve",
                    "rate_limit": {
                        "allowed": false,
                        "limit_reached": true,
                        "primary_window": {
                            "used_percent": 100,
                            "limit_window_seconds": 604800,
                            "reset_after_seconds": 240000,
                            "reset_at": 1789646165
                        }
                    }
                }
            ]
        });

        let parsed = parse_wham_usage_response(&sample);
        assert_eq!(parsed.plan_type.as_deref(), Some("plus"));
        assert!(parsed.primary_5h.is_some());
        assert_eq!(parsed.primary_5h.as_ref().unwrap().used_percent, 15);
        assert!(parsed.secondary_weekly.is_some());
        assert_eq!(parsed.secondary_weekly.as_ref().unwrap().used_percent, 50);
        assert!(parsed.reserve.is_some());
        assert!(parsed.reserve.as_ref().unwrap().limit_reached);
        assert_eq!(parsed.reserve.as_ref().unwrap().used_percent, Some(100));

        let malformed = parse_wham_usage_response(&serde_json::json!({
            "rate_limit": {
                "primary_window": {"used_percent": 110}
            }
        }));
        assert!(malformed.primary_5h.is_none());
        assert!(malformed.secondary_weekly.is_none());
    }

    #[test]
    fn active_status_requires_official_auth_mode_and_usable_identity() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("accounts.json");
        let codex_home = dir.path().join("codex_home");
        fs::create_dir_all(&codex_home).unwrap();
        let manager = AccountManager::with_credential_store(
            &store_path,
            &codex_home,
            Arc::new(codex_mp_credentials::MemoryCredentialStore::default()),
        );

        fs::write(
            manager.auth_json_path(),
            serde_json::to_vec(&serde_json::json!({
                "auth_mode": "api_key",
                "tokens": {"access_token": "not-an-official-session"}
            }))
            .unwrap(),
        )
        .unwrap();
        let status = manager.check_active_status().unwrap();
        assert!(!status.is_logged_in);
        assert_eq!(status.auth_mode.as_deref(), Some("api_key"));

        fs::write(
            manager.auth_json_path(),
            serde_json::to_vec(&serde_json::json!({
                "auth_mode": "chatgpt",
                "tokens": {"access_token": "not-enough-identity"}
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(!manager.check_active_status().unwrap().is_logged_in);
    }

    #[test]
    fn account_import_rejects_incomplete_identity_and_marks_access_only() {
        let dir = tempdir().unwrap();
        let manager = AccountManager::with_credential_store(
            dir.path().join("accounts.json"),
            dir.path().join("codex_home"),
            Arc::new(codex_mp_credentials::MemoryCredentialStore::default()),
        );

        let incomplete = manager.import_or_update_account(
            AccountTokens {
                id_token: None,
                access_token: Some("access-only".into()),
                refresh_token: None,
                account_id: None,
            },
            None,
            Some("Incomplete".into()),
        );
        assert!(matches!(incomplete, Err(AccountError::InvalidToken(_))));

        let account = manager
            .import_or_update_account(
                AccountTokens {
                    id_token: None,
                    access_token: Some("access-only".into()),
                    refresh_token: None,
                    account_id: Some("account-1".into()),
                },
                None,
                Some("Access only".into()),
            )
            .unwrap();
        assert_eq!(account.credential_state, CredentialState::AccessOnly);
    }

    #[test]
    fn usage_loader_prefers_live_auth_for_the_active_account() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("accounts.json");
        let codex_home = dir.path().join("codex_home");
        fs::create_dir_all(&codex_home).unwrap();
        let manager = AccountManager::with_credential_store(
            &store_path,
            &codex_home,
            Arc::new(codex_mp_credentials::MemoryCredentialStore::default()),
        );

        let account = manager
            .import_or_update_account(
                AccountTokens {
                    id_token: None,
                    access_token: Some("managed-access".into()),
                    refresh_token: Some("managed-refresh".into()),
                    account_id: Some("account-1".into()),
                },
                None,
                Some("Primary".into()),
            )
            .unwrap();
        fs::write(
            manager.auth_json_path(),
            serde_json::to_vec(&serde_json::json!({
                "auth_mode": "chatgpt",
                "tokens": {
                    "access_token": "live-access",
                    "refresh_token": "live-refresh",
                    "account_id": "account-1"
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let (loaded, is_active) = manager.load_account_for_usage(&account.id).unwrap();
        assert!(is_active);
        assert_eq!(loaded.tokens.access_token.as_deref(), Some("live-access"));
        assert_eq!(loaded.tokens.refresh_token.as_deref(), Some("live-refresh"));
    }

    #[test]
    fn invalidated_refresh_is_publicly_classified_as_reauth() {
        let error = AccountError::ReauthenticationRequired {
            account_id: "account-1".into(),
            code: "refresh_token_invalidated".into(),
            message: "Your session has ended".into(),
        };
        assert_eq!(error.public_code(), "reauth_required");
        assert!(error.public_message().contains("重新登录"));
        assert!(!error.public_message().contains("Your session has ended"));
    }

    #[test]
    fn test_account_crud_and_switch() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("accounts.json");
        let codex_home = dir.path().join("codex_home");
        fs::create_dir_all(&codex_home).unwrap();

        // A memory store, not the host keyring: CI runners have no keyring
        // service, so `with_paths` failed there with "No default store has been
        // set". Every other test in this module already injects one; this test
        // was the only one still depending on the environment.
        let manager = AccountManager::with_credential_store(
            &store_path,
            &codex_home,
            Arc::new(codex_mp_credentials::MemoryCredentialStore::default()),
        );

        let initial_auth = serde_json::json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": "dummy.eyJlbWFpbCI6InRlc3RAZXhhbXBsZS5jb20iLCJuYW1lIjoidGVzdHVzZXIiLCJzdWIiOiIxMjMiLCJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9wbGFuX3R5cGUiOiJwbHVzIiwiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjLTEifX0.dummy",
                "access_token": "at-1",
                "refresh_token": "rt-1",
                "account_id": "acc-1"
            }
        });
        fs::write(
            manager.auth_json_path(),
            serde_json::to_string(&initial_auth).unwrap(),
        )
        .unwrap();

        // Check active status
        let status = manager.check_active_status().unwrap();
        assert!(status.is_logged_in);
        assert_eq!(status.email.as_deref(), Some("test@example.com"));

        // Capture current
        let captured = manager
            .capture_current_auth(Some("My Primary Account".into()))
            .unwrap();
        assert_eq!(captured.name, "My Primary Account");
        assert_eq!(captured.email.as_deref(), Some("test@example.com"));

        // List accounts
        let list = manager.list_accounts().unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].is_active);

        // Add second account directly
        let acc2_tokens = AccountTokens {
            id_token: Some("dummy.eyJlbWFpbCI6InVzZXIyQGV4YW1wbGUuY29tIiwibmFtZSI6InVzZXIyIiwic3ViIjoiNDU2IiwiaHR0cHM6Ly9hcGkub3BlbmFpLmNvbS9hdXRoIjp7ImNoYXRncHRfcGxhbl90eXBlIjoiZnJlZSIsImNoYXRncHRfYWNjb3VudF9pZCI6ImFjYy0yIn19.dummy".into()),
            access_token: Some("at-2".into()),
            refresh_token: Some("rt-2".into()),
            account_id: Some("acc-2".into()),
        };
        let acc2 = manager
            .import_or_update_account(acc2_tokens, None, Some("Second Account".into()))
            .unwrap();

        // Switch to account 2
        manager.switch_to_account(&acc2.id).unwrap();

        let new_status = manager.check_active_status().unwrap();
        assert_eq!(new_status.email.as_deref(), Some("user2@example.com"));

        let list_after = manager.list_accounts().unwrap();
        assert_eq!(list_after.len(), 2);
        let item1 = list_after.iter().find(|a| a.id == captured.id).unwrap();
        let item2 = list_after.iter().find(|a| a.id == acc2.id).unwrap();
        assert!(!item1.is_active);
        assert!(item2.is_active);
    }
}
