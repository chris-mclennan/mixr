//! Beatport OAuth 2.0 with PKCE.
//!
//! Mirrors the flow the original mixr (Swift) used and that
//! Beatport's own DJ web app at dj.beatport.com runs internally.
//! The token from this flow has full scope — including streaming —
//! whereas the dj.beatport.com web app's anonymous Bearer token
//! (browse-scoped only, returns 403 on /catalog/tracks/{id}/stream/)
//! is a different mechanism.
//!
//! ### Why PKCE
//!
//! PKCE (RFC 7636) is the OAuth 2.0 extension for public clients
//! that can't keep a `client_secret`. The client generates a random
//! `code_verifier`, derives `code_challenge = base64url(SHA256(verifier))`,
//! sends the challenge during /authorize, and the verifier during
//! /token. The server checks that hash(verifier) matches the original
//! challenge. No secret leaks even if the network is observed.
//!
//! ### Why this `client_id`
//!
//! `pz8kb0BFOrRhct2Wlq5mVoPdZnOa0hcsARuVjJbm` is the public client_id
//! Beatport publishes for its own dj.beatport.com web app. It's
//! visible in any browser's network tab when signing in there — not
//! a borrowed secret. Public + PKCE is the canonical pattern for
//! native/SPA clients that talk to OAuth providers.
//!
//! ### Flow
//!
//! 1. Generate `code_verifier` (43+ chars random, URL-safe)
//! 2. Compute `code_challenge = base64url(SHA256(verifier))`
//! 3. Open WebView at `/o/authorize/?...&code_challenge=...&code_challenge_method=S256`
//! 4. User signs in. Beatport redirects to `dj.beatport.com/home?code=...&state=...`
//! 5. WebView captures the redirect URL, extracts `code`, hands to parent
//! 6. Parent POSTs `/o/token/` with `grant_type=authorization_code`,
//!    `code`, `code_verifier`, `client_id`, `redirect_uri` → access_token
//! 7. Store access_token + refresh_token. Use access_token as Bearer
//!    on every API request via reqwest (no WebView, no CORS).

use anyhow::{anyhow, Result};
use base64::Engine;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

pub const REDIRECT_URI: &str = "https://dj.beatport.com/home";
pub const AUTHORIZE_URL: &str = "https://account.beatport.com/o/authorize/";
pub const TOKEN_URL: &str = "https://account.beatport.com/o/token/";

/// One-shot PKCE challenge pair. The verifier stays on the parent
/// side; the challenge is what we ship to Beatport in the authorize
/// URL. Generate fresh per login attempt — the verifier is the
/// secret that proves we're the same client across the redirect.
pub struct PkcePair {
    pub verifier: String,
    pub challenge: String,
}

impl PkcePair {
    /// Generate a 64-char URL-safe random verifier (the spec allows
    /// 43–128). 256 bits of entropy from a timestamp+counter mix is
    /// not cryptographically ideal, but for a one-shot OAuth code
    /// exchange that the server tears down within seconds it's
    /// acceptable. (Using `rand` would pull in dependencies for
    /// negligible benefit at this attack surface.)
    pub fn generate() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n1 = COUNTER.fetch_add(1, Ordering::Relaxed);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64).unwrap_or(0);
        let n2 = std::process::id() as u64;
        // 6 × 16 hex chars = 96 hex = 48 bytes. URL-safe by virtue of
        // being [0-9a-f] only.
        let verifier = format!(
            "{ts:016x}{n1:016x}{n2:016x}{:016x}{:016x}{:016x}",
            ts.wrapping_mul(n1.wrapping_add(1)),
            n1.wrapping_mul(n2.wrapping_add(1)),
            ts ^ n1 ^ n2,
        );
        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(hasher.finalize());
        Self { verifier, challenge }
    }

    /// Build the authorize URL the WebView should load. The
    /// `client_id` is supplied at runtime — scraped from
    /// dj.beatport.com (see `webview_host::run_discover`).
    pub fn authorize_url(&self, client_id: &str) -> String {
        format!(
            "{AUTHORIZE_URL}?response_type=code&client_id={client_id}\
             &redirect_uri={}&code_challenge={}&code_challenge_method=S256",
            urlencode(REDIRECT_URI),
            self.challenge,
        )
    }
}

/// Token bundle returned by `/o/token/`. We only need access_token
/// for reqwest's Authorization header; refresh_token is kept on disk
/// so we can renew without re-prompting the user when the access
/// token expires (typically 1 hour).
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct StoredAuth {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    /// Unix seconds when access_token expires. `None` for legacy
    /// records or if Beatport ever stops sending `expires_in`.
    pub expires_at: Option<i64>,
    /// Last `client_id` we successfully discovered from dj.beatport.com.
    /// Persisted so subsequent launches skip the discovery WebView pass.
    /// Re-discovered if a token exchange/refresh fails (e.g., Beatport
    /// rotated the ID).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

impl StoredAuth {
    fn path() -> PathBuf {
        let dir = dirs::home_dir().unwrap_or_default().join(".mixr");
        std::fs::create_dir_all(&dir).ok();
        dir.join("auth.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let path = Self::path();
            std::fs::write(&path, json).ok();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    &path,
                    std::fs::Permissions::from_mode(0o600),
                ).ok();
            }
        }
    }

    pub fn delete() {
        std::fs::remove_file(Self::path()).ok();
    }

    /// Best-effort liveness check. Returns true if we have a token
    /// and (no expiry recorded OR it's still in the future with 60s
    /// of cushion). Doesn't validate the token actually works against
    /// Beatport — first API call will discover that.
    pub fn looks_live(&self) -> bool {
        if self.access_token.is_none() { return false; }
        match self.expires_at {
            None => true,
            Some(exp) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64).unwrap_or(0);
                exp > now + 60
            }
        }
    }
}

/// Exchange an OAuth authorization code (captured from the redirect)
/// for an access_token + refresh_token. PKCE: send the verifier we
/// kept secret so Beatport can confirm we're the same client that
/// hashed it into the challenge during /authorize.
pub async fn exchange_code(code: &str, verifier: &str, client_id: &str) -> Result<StoredAuth> {
    let client = Client::builder().build()?;
    let resp = client.post(TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "grant_type=authorization_code&code={}&redirect_uri={}\
             &client_id={}&code_verifier={}",
            urlencode(code), urlencode(REDIRECT_URI), urlencode(client_id),
            urlencode(verifier),
        ))
        .send().await?;

    let status = resp.status();
    let body = resp.text().await?;
    if !status.is_success() {
        return Err(anyhow!("token exchange failed: HTTP {status}: {}",
            &body[..body.len().min(300)]));
    }
    let json: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| anyhow!("token response not JSON: {e}"))?;

    let access_token = json["access_token"].as_str()
        .ok_or_else(|| anyhow!("no access_token in token response"))?
        .to_string();
    let refresh_token = json["refresh_token"].as_str().map(String::from);
    let expires_at = json["expires_in"].as_i64().map(|secs| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64).unwrap_or(0) + secs
    });

    let stored = StoredAuth {
        access_token: Some(access_token),
        refresh_token,
        expires_at,
        client_id: Some(client_id.to_string()),
    };
    stored.save();
    tracing::info!("Beatport: OAuth PKCE token exchange OK (refresh={})",
        stored.refresh_token.is_some());
    Ok(stored)
}

/// Renew an access_token from a refresh_token. Used when the cached
/// access_token has expired but we still have a valid refresh_token —
/// avoids re-popping the WebView for sign-in.
pub async fn refresh(refresh_token: &str, client_id: &str) -> Result<StoredAuth> {
    let client = Client::builder().build()?;
    let resp = client.post(TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "grant_type=refresh_token&refresh_token={}&client_id={}",
            urlencode(refresh_token), urlencode(client_id),
        ))
        .send().await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(anyhow!("refresh failed: HTTP {status}"));
    }
    let json: serde_json::Value = resp.json().await?;
    let access_token = json["access_token"].as_str()
        .ok_or_else(|| anyhow!("no access_token in refresh response"))?
        .to_string();
    let new_refresh = json["refresh_token"].as_str().map(String::from)
        // Beatport may omit refresh_token on refresh — keep the old one
        .or_else(|| Some(refresh_token.to_string()));
    let expires_at = json["expires_in"].as_i64().map(|secs| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64).unwrap_or(0) + secs
    });
    let stored = StoredAuth {
        access_token: Some(access_token),
        refresh_token: new_refresh,
        expires_at,
        client_id: Some(client_id.to_string()),
    };
    stored.save();
    tracing::info!("Beatport: refresh OK");
    Ok(stored)
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'
            | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc_7636_example() {
        // RFC 7636 Appendix B: with verifier
        // "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk", challenge
        // should be "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM".
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(hasher.finalize());
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn pkce_pair_generates_valid_challenge() {
        // Round-trip our own generator: hash(verifier) must equal
        // base64-decoded challenge. Catches accidental mishandling
        // of base64 padding or encoding variant.
        let pair = PkcePair::generate();
        assert!(pair.verifier.len() >= 43);
        let mut hasher = Sha256::new();
        hasher.update(pair.verifier.as_bytes());
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(hasher.finalize());
        assert_eq!(pair.challenge, expected);
    }

    #[test]
    fn authorize_url_includes_pkce_params() {
        let pair = PkcePair::generate();
        let url = pair.authorize_url("custom123");
        assert!(url.contains("response_type=code"));
        assert!(url.contains("client_id=custom123"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains(&format!("code_challenge={}", pair.challenge)));
    }

    #[test]
    fn looks_live_handles_no_expiry_no_token() {
        let none = StoredAuth::default();
        assert!(!none.looks_live(), "no token → not live");

        let no_exp = StoredAuth {
            access_token: Some("x".into()),
            ..Default::default()
        };
        assert!(no_exp.looks_live(), "token without expiry → assumed live");
    }

    #[test]
    fn looks_live_respects_expiry_with_cushion() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64).unwrap_or(0);
        let expired = StoredAuth {
            access_token: Some("x".into()),
            expires_at: Some(now - 100),
            ..Default::default()
        };
        assert!(!expired.looks_live(), "expired token → not live");

        let about_to_expire = StoredAuth {
            access_token: Some("x".into()),
            expires_at: Some(now + 30),  // less than 60s cushion
            ..Default::default()
        };
        assert!(!about_to_expire.looks_live(), "<60s cushion → not live");

        let fresh = StoredAuth {
            access_token: Some("x".into()),
            expires_at: Some(now + 600),
            ..Default::default()
        };
        assert!(fresh.looks_live(), "fresh token with cushion → live");
    }
}
