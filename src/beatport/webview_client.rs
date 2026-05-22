//! Parent-side OAuth code capture.
//!
//! Spawns `mixr --webview-host <authorize_url>` as a child process,
//! waits for one line of JSON containing the captured OAuth code (or
//! an error), and returns it. The child is a one-shot — it exits as
//! soon as the redirect is observed, no long-running proxy.

use anyhow::{anyhow, Result};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

/// Result of an OAuth WebView capture round-trip.
#[derive(Debug)]
pub struct CapturedCode {
    pub code: String,
    #[allow(dead_code)] // state is unused right now; we'll need it if we add CSRF protection
    pub state: Option<String>,
}

/// Discover the OAuth `client_id` Beatport's own DJ web app uses by
/// loading dj.beatport.com in a hidden WebView and scraping the page
/// for any `client_id=...` reference. Used at first launch (or when
/// a cached value stops working) so we don't have to embed Beatport's
/// `client_id` in our binary. Returns the discovered string, or an
/// error if the discovery WebView times out or crashes.
pub fn discover_client_id() -> Result<String> {
    let exe = std::env::current_exe()?;
    let mut child = Command::new(exe)
        .arg("--webview-discover")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;

    let stdout = child.stdout.take()
        .ok_or_else(|| anyhow!("child stdout missing"))?;
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let _ = child.wait();

    let json: serde_json::Value = serde_json::from_str(line.trim())
        .map_err(|e| anyhow!("malformed discover response: {e}: {line:?}"))?;
    if let Some(err) = json.get("error").and_then(|v| v.as_str()) {
        return Err(anyhow!("client_id discovery: {err}"));
    }
    json.get("client_id").and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow!("no client_id in discover response"))
}

/// Spawn the WebView capture subprocess and block until it returns
/// the OAuth code (or fails). The subprocess pops a Beatport sign-in
/// window (or short-circuits silently on already-signed-in cookies).
///
/// Synchronous because we want the TUI to wait for sign-in to complete
/// before proceeding — there's nothing useful to do until we have the
/// code anyway.
pub fn capture_oauth_code(authorize_url: &str) -> Result<CapturedCode> {
    let exe = std::env::current_exe()?;
    let mut child = Command::new(exe)
        .arg("--webview-host")
        .arg(authorize_url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;

    let stdout = child.stdout.take()
        .ok_or_else(|| anyhow!("child stdout missing"))?;
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line)?;

    let _ = child.wait();

    let json: serde_json::Value = serde_json::from_str(line.trim())
        .map_err(|e| anyhow!("malformed capture response: {e}: {line:?}"))?;

    if let Some(err) = json.get("error").and_then(|v| v.as_str()) {
        return Err(anyhow!("OAuth capture: {err}"));
    }
    let code = json.get("code").and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("no code in capture response"))?
        .to_string();
    let state = json.get("state").and_then(|v| v.as_str()).map(String::from);
    Ok(CapturedCode { code, state })
}
