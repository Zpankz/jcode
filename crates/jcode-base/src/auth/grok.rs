//! Grok Build (SuperGrok subscription) OAuth credential handling.
//!
//! Grok Build authenticates through xAI's OIDC provider (`https://auth.x.ai`).
//! The official Grok CLI persists tokens to `~/.grok/auth.json` as a
//! dict-of-dicts keyed by `<oidc_issuer>::<oidc_client_id>`, where each entry
//! carries a `key` (JWT access token), a `refresh_token`, an `expires_at`
//! (RFC3339), plus the OIDC issuer / client id used for refresh.
//!
//! jcode reuses those credentials so a user who is logged into the Grok CLI is
//! automatically logged into jcode's Grok Build provider. When the access token
//! has expired we transparently refresh it against the xAI token endpoint and
//! write the rotated tokens back to `~/.grok/auth.json`.
//!
//! Resolution order for the bearer token:
//!   1. `GROK_CODE_XAI_API_KEY` env (CI / headless)
//!   2. `~/.grok/auth.json` OAuth entry (refreshing if expired)
//!
//! The resolved bearer is materialized into the Grok Build provider env file
//! under `GROK_CODE_XAI_API_KEY` so the OpenAI-compatible transport can read it.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Env var that overrides the OAuth flow with a directly-provided bearer token.
pub const GROK_API_KEY_ENV: &str = "GROK_CODE_XAI_API_KEY";
/// Provider env file the OpenAI-compatible transport reads the bearer from.
pub const GROK_ENV_FILE: &str = "grok-build.env";
/// Default xAI OIDC issuer (used when the auth.json entry omits it).
const DEFAULT_OIDC_ISSUER: &str = "https://auth.x.ai";
/// Path to the xAI token endpoint, relative to the issuer.
const TOKEN_PATH: &str = "/oauth2/token";
/// Refresh the access token this many seconds before it actually expires.
const EXPIRY_SKEW_SECONDS: i64 = 120;

/// A single OAuth entry inside `~/.grok/auth.json`.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct GrokAuthEntry {
    /// JWT access token (the field is literally named `key` in auth.json).
    key: String,
    #[serde(default)]
    refresh_token: Option<String>,
    /// RFC3339 expiry timestamp, e.g. `2026-05-30T13:49:13.337828Z`.
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    oidc_issuer: Option<String>,
    #[serde(default)]
    oidc_client_id: Option<String>,
    /// Preserve any unknown fields when rewriting the file.
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

impl GrokAuthEntry {
    fn issuer(&self) -> &str {
        self.oidc_issuer
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(DEFAULT_OIDC_ISSUER)
    }

    /// True when the access token is expired or within the refresh skew window.
    fn is_expired(&self) -> bool {
        let Some(raw) = self.expires_at.as_deref() else {
            // No expiry recorded: treat as valid and let the proxy reject it
            // if stale, rather than forcing a refresh we cannot reason about.
            return false;
        };
        match chrono::DateTime::parse_from_rfc3339(raw.trim()) {
            Ok(expires) => {
                let now = chrono::Utc::now().timestamp();
                expires.timestamp() <= now + EXPIRY_SKEW_SECONDS
            }
            // Unparseable expiry: be conservative and attempt a refresh.
            Err(_) => true,
        }
    }
}

/// Token endpoint response from `https://auth.x.ai/oauth2/token`.
#[derive(Debug, Deserialize)]
struct GrokTokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

/// Location of the Grok CLI auth file (`~/.grok/auth.json`), env-overridable.
fn grok_auth_path() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("JCODE_GROK_AUTH_FILE") {
        let path = PathBuf::from(explicit);
        if !path.as_os_str().is_empty() {
            return Some(path);
        }
    }
    dirs::home_dir().map(|home| home.join(".grok").join("auth.json"))
}

/// Read all OAuth entries from `~/.grok/auth.json`, preserving their keys.
fn read_auth_file(path: &PathBuf) -> Result<BTreeMap<String, GrokAuthEntry>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read Grok auth file at {}", path.display()))?;
    let parsed: BTreeMap<String, GrokAuthEntry> = serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse Grok auth file at {}", path.display()))?;
    Ok(parsed)
}

/// Select the most usable OAuth entry: prefer a non-empty access token, and
/// among those prefer the one that is not (yet) expired.
fn select_entry(entries: &BTreeMap<String, GrokAuthEntry>) -> Option<(String, GrokAuthEntry)> {
    let mut best: Option<(String, GrokAuthEntry)> = None;
    for (id, entry) in entries {
        if entry.key.trim().is_empty() {
            continue;
        }
        let entry_fresh = !entry.is_expired();
        match &best {
            None => best = Some((id.clone(), entry.clone())),
            Some((_, current)) => {
                // Upgrade to a fresh entry if the current pick is expired.
                if entry_fresh && current.is_expired() {
                    best = Some((id.clone(), entry.clone()));
                }
            }
        }
    }
    best
}

/// Refresh an expired Grok access token against the xAI token endpoint.
async fn refresh_entry(entry: &GrokAuthEntry) -> Result<GrokTokenResponse> {
    let refresh_token = entry
        .refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("Grok auth entry has no refresh_token; re-run `grok` to log in")?;
    let client_id = entry
        .oidc_client_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("Grok auth entry has no oidc_client_id; re-run `grok` to log in")?;
    let token_url = format!("{}{}", entry.issuer().trim_end_matches('/'), TOKEN_PATH);

    let client = crate::provider::shared_http_client();
    let resp = client
        .post(&token_url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await
        .context("Failed to refresh Grok OAuth token")?;

    if !resp.status().is_success() {
        let body = crate::util::http_error_body(resp, "HTTP error").await;
        anyhow::bail!("Grok token refresh failed: {}", body.trim());
    }

    resp.json::<GrokTokenResponse>()
        .await
        .context("Failed to parse Grok token refresh response")
}

/// Persist a refreshed token back into the auth.json entry, preserving all
/// other fields and entries so we stay compatible with the Grok CLI.
fn persist_refreshed(
    path: &PathBuf,
    entries: &mut BTreeMap<String, GrokAuthEntry>,
    entry_id: &str,
    token: &GrokTokenResponse,
) -> Result<()> {
    let Some(entry) = entries.get_mut(entry_id) else {
        return Ok(());
    };
    entry.key = token.access_token.clone();
    if let Some(refresh) = token
        .refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        entry.refresh_token = Some(refresh.to_string());
    }
    if let Some(expires_in) = token.expires_in {
        let new_expiry = chrono::Utc::now() + chrono::Duration::seconds(expires_in);
        entry.expires_at = Some(new_expiry.to_rfc3339());
    }

    let serialized =
        serde_json::to_string_pretty(entries).context("Failed to serialize Grok auth file")?;
    std::fs::write(path, serialized)
        .with_context(|| format!("Failed to write Grok auth file at {}", path.display()))?;
    Ok(())
}

/// Resolve a usable Grok Build bearer token, refreshing the OAuth credential if
/// needed. Returns `Ok(None)` when no credential source is configured.
pub async fn resolve_access_token() -> Result<Option<String>> {
    // 1. Direct env override (CI / headless).
    if let Some(token) = std::env::var(GROK_API_KEY_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return Ok(Some(token));
    }

    // 2. OAuth credentials from the Grok CLI's auth.json.
    let Some(path) = grok_auth_path() else {
        return Ok(None);
    };
    if !path.exists() {
        return Ok(None);
    }

    let mut entries = read_auth_file(&path)?;
    let Some((entry_id, entry)) = select_entry(&entries) else {
        return Ok(None);
    };

    if !entry.is_expired() {
        return Ok(Some(entry.key.clone()));
    }

    // Token expired: refresh and persist.
    let token = refresh_entry(&entry).await?;
    let access_token = token.access_token.clone();
    persist_refreshed(&path, &mut entries, &entry_id, &token)?;
    Ok(Some(access_token))
}

/// Materialize a resolved Grok bearer into the provider env file so the
/// OpenAI-compatible transport can read it via `GROK_CODE_XAI_API_KEY`.
pub async fn materialize_access_token() -> Result<String> {
    let token = resolve_access_token().await?.context(
        "No Grok Build credentials found. Log in with the Grok CLI (`grok`) so \
         ~/.grok/auth.json exists, or set GROK_CODE_XAI_API_KEY.",
    )?;
    crate::provider_catalog::save_env_value_to_env_file(
        GROK_API_KEY_ENV,
        GROK_ENV_FILE,
        Some(&token),
    )?;
    Ok(token)
}

/// Whether any Grok Build credential source is configured (without refreshing).
pub fn has_credentials() -> bool {
    if std::env::var(GROK_API_KEY_ENV)
        .ok()
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
    {
        return true;
    }
    grok_auth_path().map(|path| path.exists()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str, expires_at: Option<&str>) -> GrokAuthEntry {
        GrokAuthEntry {
            key: key.to_string(),
            refresh_token: Some("refresh".to_string()),
            expires_at: expires_at.map(|s| s.to_string()),
            oidc_issuer: Some(DEFAULT_OIDC_ISSUER.to_string()),
            oidc_client_id: Some("client".to_string()),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn detects_expired_token() {
        let past = "2000-01-01T00:00:00Z";
        assert!(entry("k", Some(past)).is_expired());
    }

    #[test]
    fn detects_fresh_token() {
        let future = (chrono::Utc::now() + chrono::Duration::hours(2)).to_rfc3339();
        assert!(!entry("k", Some(&future)).is_expired());
    }

    #[test]
    fn missing_expiry_is_not_expired() {
        assert!(!entry("k", None).is_expired());
    }

    #[test]
    fn unparseable_expiry_forces_refresh() {
        assert!(entry("k", Some("not-a-date")).is_expired());
    }

    #[test]
    fn select_prefers_fresh_entry() {
        let future = (chrono::Utc::now() + chrono::Duration::hours(2)).to_rfc3339();
        let mut entries = BTreeMap::new();
        entries.insert(
            "expired".to_string(),
            entry("old", Some("2000-01-01T00:00:00Z")),
        );
        entries.insert("fresh".to_string(), entry("new", Some(&future)));
        let (id, _) = select_entry(&entries).expect("an entry");
        assert_eq!(id, "fresh");
    }

    #[test]
    fn select_skips_empty_keys() {
        let mut entries = BTreeMap::new();
        entries.insert("empty".to_string(), entry("", None));
        assert!(select_entry(&entries).is_none());
    }

    #[test]
    fn issuer_falls_back_to_default() {
        let mut e = entry("k", None);
        e.oidc_issuer = None;
        assert_eq!(e.issuer(), DEFAULT_OIDC_ISSUER);
    }
}
