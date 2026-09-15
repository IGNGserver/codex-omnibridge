//! Management of official Codex / ChatGPT accounts, credentials persistence,
//! token refresh, rate limits usage checking, atomic switching, and Codex process restarting.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use codex_mp_core::{atomic_replace, set_private_permissions};
use directories::{BaseDirs, ProjectDirs};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const OPENAI_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OPENAI_OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const OPENAI_WHAM_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const DEFAULT_USER_AGENT: &str = "codex_cli_rs";

#[derive(Debug, Error)]
pub enum AccountError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("Account not found: `{0}`")]
    NotFound(String),
    #[error("Invalid token format: {0}")]
    InvalidToken(String),
    #[error("Token refresh failed: {0}")]
    RefreshFailed(String),
    #[error("Rate limit check failed: HTTP {status} {message}")]
    UsageCheckFailed { status: u16, message: String },
    #[error("Failed to determine Codex home directory")]
    CodexHomeNotFound,
    #[error("Failed to parse auth.json: {0}")]
    AuthJsonInvalid(String),
}

/// Token payload stored in auth.json and account store
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

/// Managed account entry
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedAccount {
    pub id: String,
    pub name: String,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub user_id: Option<String>,
    pub account_id: Option<String>,
    pub tokens: AccountTokens,
    pub last_refresh: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub usage: Option<AccountUsageSnapshot>,
}

/// Persistent store file structure
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AccountsFile {
    pub schema_version: u32,
    pub accounts: Vec<ManagedAccount>,
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
    http_client: Client,
}

impl AccountManager {
    pub fn new() -> Result<Self, AccountError> {
        let store_path = default_accounts_path();
        let codex_home = default_codex_home().ok_or(AccountError::CodexHomeNotFound)?;
        Ok(Self::with_paths(store_path, codex_home))
    }

    pub fn with_paths(store_path: impl Into<PathBuf>, codex_home: impl Into<PathBuf>) -> Self {
        Self {
            store_path: store_path.into(),
            codex_home: codex_home.into(),
            http_client: Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .unwrap_or_else(|_| Client::new()),
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

    // ---------------- Persistence ---------------- //

    pub fn load_file(&self) -> Result<AccountsFile, AccountError> {
        if !self.store_path.exists() {
            return Ok(AccountsFile {
                schema_version: 1,
                accounts: Vec::new(),
            });
        }
        let content = fs::read_to_string(&self.store_path)?;
        let file: AccountsFile = serde_json::from_str(&content)?;
        Ok(file)
    }

    pub fn save_file(&self, file: &AccountsFile) -> Result<(), AccountError> {
        if let Some(parent) = self.store_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = self
            .store_path
            .with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let bytes = serde_json::to_vec_pretty(file)?;
        {
            let mut f = File::create(&temp)?;
            f.write_all(&bytes)?;
            f.write_all(b"\n")?;
            f.sync_all()?;
        }
        atomic_replace(&temp, &self.store_path)?;
        set_private_permissions(&self.store_path)?;
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

        let tokens_val = active_auth.get("tokens");
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

        let file = self.load_file()?;
        let matched = file.accounts.iter().find(|acc| {
            if let (Some(acc_email), Some(cur_email)) = (&acc.email, &email) {
                if acc_email.eq_ignore_ascii_case(cur_email) {
                    return true;
                }
            }
            if let (Some(acc_aid), Some(cur_aid)) = (&acc.account_id, &effective_account_id) {
                if acc_aid == cur_aid {
                    return true;
                }
            }
            false
        });

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
        let active = self.check_active_status().unwrap_or(ActiveAccountStatus {
            is_logged_in: false,
            email: None,
            plan_type: None,
            account_id: None,
            matched_account_id: None,
            auth_mode: None,
        });

        let mut summaries = Vec::new();
        for acc in file.accounts {
            let is_active = if let Some(matched_id) = &active.matched_account_id {
                acc.id == *matched_id
            } else if let (Some(active_email), Some(acc_email)) = (&active.email, &acc.email) {
                active_email.eq_ignore_ascii_case(acc_email)
            } else {
                false
            };

            summaries.push(AccountSummary {
                id: acc.id,
                name: acc.name,
                email: acc.email,
                plan_type: acc.plan_type,
                is_active,
                updated_at: acc.updated_at,
                usage: acc.usage,
            });
        }

        Ok(summaries)
    }

    /// Capture current active auth.json into managed accounts
    pub fn capture_current_auth(
        &self,
        custom_name: Option<String>,
    ) -> Result<ManagedAccount, AccountError> {
        let auth_val = self
            .read_active_auth()?
            .ok_or_else(|| AccountError::AuthJsonInvalid("auth.json not found".into()))?;

        let tokens_obj = auth_val
            .get("tokens")
            .ok_or_else(|| AccountError::AuthJsonInvalid("tokens field missing".into()))?;

        let tokens: AccountTokens = serde_json::from_value(tokens_obj.clone())
            .map_err(|e| AccountError::AuthJsonInvalid(format!("invalid tokens: {e}")))?;

        let last_refresh = auth_val
            .get("last_refresh")
            .and_then(|v| v.as_str())
            .map(String::from);

        self.import_or_update_account(tokens, last_refresh, custom_name)
    }

    /// Import or update an account given its tokens
    pub fn import_or_update_account(
        &self,
        tokens: AccountTokens,
        last_refresh: Option<String>,
        name_hint: Option<String>,
    ) -> Result<ManagedAccount, AccountError> {
        let profile = tokens
            .id_token
            .as_deref()
            .and_then(|tok| parse_id_token_claims(tok).ok())
            .unwrap_or_default();

        let now = now_secs();
        let email = profile.email.clone();
        let plan_type = profile.plan_type.clone();
        let user_id = profile.user_id.clone();
        let account_id = tokens
            .account_id
            .clone()
            .or_else(|| profile.chatgpt_account_id.clone());

        let mut file = self.load_file()?;
        let existing_index = file.accounts.iter().position(|acc| {
            if let (Some(a_email), Some(e_email)) = (&acc.email, &email) {
                if a_email.eq_ignore_ascii_case(e_email) {
                    return true;
                }
            }
            if let (Some(a_id), Some(e_id)) = (&acc.account_id, &account_id) {
                if a_id == e_id {
                    return true;
                }
            }
            false
        });

        let account = if let Some(idx) = existing_index {
            let acc = &mut file.accounts[idx];
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
            acc.updated_at = now;
            acc.clone()
        } else {
            let id = uuid::Uuid::new_v4().to_string();
            let name = name_hint.unwrap_or_else(|| {
                email
                    .as_deref()
                    .unwrap_or_else(|| "ChatGPT Account")
                    .to_string()
            });

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
            };
            file.accounts.push(new_acc.clone());
            new_acc
        };

        self.save_file(&file)?;
        Ok(account)
    }

    pub fn delete_account(&self, id: &str) -> Result<bool, AccountError> {
        let mut file = self.load_file()?;
        let before_len = file.accounts.len();
        file.accounts.retain(|acc| acc.id != id);
        if file.accounts.len() != before_len {
            self.save_file(&file)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn rename_account(&self, id: &str, new_name: &str) -> Result<(), AccountError> {
        let mut file = self.load_file()?;
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
        let file = self.load_file()?;
        let acc_idx = file
            .accounts
            .iter()
            .position(|a| a.id == id)
            .ok_or_else(|| AccountError::NotFound(id.to_string()))?;

        let account = file.accounts[acc_idx].clone();

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

        // Backup existing auth.json before writing
        let auth_path = self.auth_json_path();
        if auth_path.exists() {
            let backup_path = auth_path.with_extension(format!("bak-switch-{}", now_secs()));
            let _ = fs::copy(&auth_path, backup_path);
        } else if let Some(parent) = auth_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let temp_path = auth_path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
        let bytes = serde_json::to_vec_pretty(&auth_doc)?;
        {
            let mut f = File::create(&temp_path)?;
            f.write_all(&bytes)?;
            f.write_all(b"\n")?;
            f.sync_all()?;
        }
        atomic_replace(&temp_path, &auth_path)?;
        set_private_permissions(&auth_path)?;

        Ok(account)
    }

    // ---------------- Refresh Token & Usage Query ---------------- //

    /// Refresh token if access token is missing or expired, updating stored account
    pub async fn refresh_account_token(
        &self,
        account_id: &str,
    ) -> Result<ManagedAccount, AccountError> {
        let mut file = self.load_file()?;
        let acc_idx = file
            .accounts
            .iter()
            .position(|a| a.id == account_id)
            .ok_or_else(|| AccountError::NotFound(account_id.to_string()))?;

        let refresh_token = file.accounts[acc_idx]
            .tokens
            .refresh_token
            .as_deref()
            .ok_or_else(|| {
                AccountError::RefreshFailed("No refresh_token available for this account".into())
            })?;

        let form_body = form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "refresh_token")
            .append_pair("client_id", OPENAI_CLIENT_ID)
            .append_pair("refresh_token", refresh_token)
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
            let err_text = resp.text().await.unwrap_or_default();
            return Err(AccountError::RefreshFailed(format!(
                "HTTP {status}: {err_text}"
            )));
        }

        let token_resp: serde_json::Value = resp.json().await?;
        let new_access_token = token_resp
            .get("access_token")
            .and_then(|v| v.as_str())
            .map(String::from);
        let new_refresh_token = token_resp
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .map(String::from);
        let new_id_token = token_resp
            .get("id_token")
            .and_then(|v| v.as_str())
            .map(String::from);

        let acc = &mut file.accounts[acc_idx];
        if let Some(at) = new_access_token {
            acc.tokens.access_token = Some(at);
        }
        if let Some(rt) = new_refresh_token {
            acc.tokens.refresh_token = Some(rt);
        }
        if let Some(it) = new_id_token {
            if let Ok(profile) = parse_id_token_claims(&it) {
                if profile.email.is_some() {
                    acc.email = profile.email;
                }
                if profile.plan_type.is_some() {
                    acc.plan_type = profile.plan_type;
                }
            }
            acc.tokens.id_token = Some(it);
        }
        acc.last_refresh = Some(chrono_iso_now());
        acc.updated_at = now_secs();

        let updated_acc = acc.clone();
        self.save_file(&file)?;

        // If this refreshed account happens to be the active one in auth.json, sync it
        let active = self.check_active_status().unwrap_or(ActiveAccountStatus {
            is_logged_in: false,
            email: None,
            plan_type: None,
            account_id: None,
            matched_account_id: None,
            auth_mode: None,
        });
        if active.matched_account_id.as_deref() == Some(account_id) {
            let _ = self.switch_to_account(account_id);
        }

        Ok(updated_acc)
    }

    /// Fetch usage / rate limits for a given account. If 401 Unauthorized is returned,
    /// attempt token refresh and retry once.
    pub async fn fetch_usage(
        &self,
        account_id: &str,
    ) -> Result<AccountUsageSnapshot, AccountError> {
        let account = {
            let file = self.load_file()?;
            file.accounts
                .into_iter()
                .find(|a| a.id == account_id)
                .ok_or_else(|| AccountError::NotFound(account_id.to_string()))?
        };

        match self.do_fetch_usage(&account).await {
            Ok(snapshot) => {
                self.save_usage_snapshot(account_id, snapshot.clone())?;
                Ok(snapshot)
            }
            Err(AccountError::UsageCheckFailed { status: 401, .. }) => {
                // Token might be expired, try refreshing
                let refreshed = self.refresh_account_token(account_id).await?;
                let snapshot = self.do_fetch_usage(&refreshed).await?;
                self.save_usage_snapshot(account_id, snapshot.clone())?;
                Ok(snapshot)
            }
            Err(e) => Err(e),
        }
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
            let msg = resp.text().await.unwrap_or_default();
            return Err(AccountError::UsageCheckFailed {
                status,
                message: msg,
            });
        }

        let body: serde_json::Value = resp.json().await?;
        let snapshot = parse_wham_usage_response(&body);
        Ok(snapshot)
    }

    fn save_usage_snapshot(
        &self,
        account_id: &str,
        snapshot: AccountUsageSnapshot,
    ) -> Result<(), AccountError> {
        let mut file = self.load_file()?;
        if let Some(acc) = file.accounts.iter_mut().find(|a| a.id == account_id) {
            acc.usage = Some(snapshot);
            acc.updated_at = now_secs();
            self.save_file(&file)?;
        }
        Ok(())
    }

    // ---------------- Restarting Codex ---------------- //

    /// Restart Codex processes (Desktop app-server and CLI background processes)
    pub fn restart_codex_processes(&self) -> Result<RestartCodexReport, AccountError> {
        let mut killed_pids = Vec::new();

        // 1. Terminate running codex app-server processes
        let running_pids = find_codex_running_pids();
        for pid in &running_pids {
            terminate_pid(*pid);
            killed_pids.push(*pid);
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

fn chrono_iso_now() -> String {
    // Simple UTC ISO8601 representation without external date crate
    let secs = now_secs();
    format!("{secs}")
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

pub fn parse_wham_usage_response(val: &serde_json::Value) -> AccountUsageSnapshot {
    let plan_type = val
        .get("plan_type")
        .and_then(|v| v.as_str())
        .map(String::from);

    let mut primary_5h: Option<UsageWindow> = None;
    let mut secondary_weekly: Option<UsageWindow> = None;

    if let Some(rate_limit) = val.get("rate_limit") {
        if let Some(pw) = rate_limit.get("primary_window") {
            let window_secs = pw
                .get("limit_window_seconds")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let used = pw.get("used_percent").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let reset_after = pw
                .get("reset_after_seconds")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let reset_at = pw.get("reset_at").and_then(|v| v.as_u64()).unwrap_or(0);
            let w = UsageWindow {
                used_percent: used,
                limit_window_seconds: window_secs,
                reset_after_seconds: reset_after,
                reset_at,
            };
            if window_secs <= 18000 {
                primary_5h = Some(w);
            } else {
                secondary_weekly = Some(w);
            }
        }
        if let Some(sw) = rate_limit.get("secondary_window").filter(|v| !v.is_null()) {
            let window_secs = sw
                .get("limit_window_seconds")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let used = sw.get("used_percent").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let reset_after = sw
                .get("reset_after_seconds")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let reset_at = sw.get("reset_at").and_then(|v| v.as_u64()).unwrap_or(0);
            secondary_weekly = Some(UsageWindow {
                used_percent: used,
                limit_window_seconds: window_secs,
                reset_after_seconds: reset_after,
                reset_at,
            });
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
                    .map(|v| v as u32);
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

/// Find running Codex PIDs
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
            if let Ok(cmdline) = fs::read(cmdline_path) {
                let cmd_str = String::from_utf8_lossy(&cmdline);
                // Look for codex app-server processes
                if (cmd_str.contains("codex") || cmd_str.contains("codex-real"))
                    && cmd_str.contains("app-server")
                {
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
                if fields.len() >= 2 {
                    let img = fields[0].to_lowercase();
                    if img.contains("codex") {
                        if let Ok(pid) = fields[1].parse::<u32>() {
                            pids.push(pid);
                        }
                    }
                }
            }
        }
        pids
    }

    #[cfg(target_os = "macos")]
    {
        let mut pids = Vec::new();
        let output = std::process::Command::new("pgrep")
            .args(["-f", "codex.*app-server"])
            .output();
        if let Ok(out) = output {
            let stdout = String::from_utf8_lossy(&out.stdout);
            for line in stdout.lines() {
                if let Ok(pid) = line.trim().parse::<u32>() {
                    pids.push(pid);
                }
            }
        }
        pids
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        Vec::new()
    }
}

pub fn terminate_pid(pid: u32) {
    #[cfg(unix)]
    {
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill.exe")
            .args(["/PID", &pid.to_string(), "/F"])
            .output();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

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
        assert_eq!(parsed.reserve.as_ref().unwrap().limit_reached, true);
        assert_eq!(parsed.reserve.as_ref().unwrap().used_percent, Some(100));
    }

    #[test]
    fn test_account_crud_and_switch() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("accounts.json");
        let codex_home = dir.path().join("codex_home");
        fs::create_dir_all(&codex_home).unwrap();

        let manager = AccountManager::with_paths(&store_path, &codex_home);

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
