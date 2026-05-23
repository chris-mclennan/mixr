//! WebView subprocess for OAuth PKCE code capture.
//!
//! This subprocess pops a Beatport sign-in window, watches for the
//! redirect to `dj.beatport.com/home?code=...&state=...`, prints the
//! captured code on stdout, and exits. The parent process spawns
//! this with `mixr --webview-host AUTHORIZE_URL` and reads one line
//! of JSON back: `{"code":"...","state":"..."}`.
//!
//! ### Why a subprocess
//!
//! `tao::EventLoop::run` owns the main thread on macOS (NSApp's
//! event loop never returns). The TUI also wants the main thread.
//! Putting them in the same process would force one to give up.
//! Running the WebView in a child process keeps both free to own
//! their own main thread.
//!
//! ### Why we don't need the long-running proxy anymore
//!
//! Earlier iterations routed all Beatport API calls through this
//! WebView via JS fetch+credentials. That hit WKWebView's CORS
//! quirks (api.beatport.com responds Access-Control-Allow-Origin: *
//! which is incompatible with credentials). The OAuth PKCE token
//! has full scope, so once we have it the parent talks to
//! api.beatport.com directly via reqwest — no CORS, no proxy.

use anyhow::Result;
use std::io::Write;

use tao::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop, EventLoopBuilder},
    window::WindowBuilder,
    dpi::LogicalSize,
};
#[cfg(target_os = "macos")]
use wry::WebViewBuilderExtDarwin;
use wry::WebViewBuilder;

/// Stable identifier for the WebKit data store. Lets the user stay
/// signed in across launches — Beatport's session cookies persist
/// in this store, so the next OAuth round trips through the page
/// without re-prompting (Beatport's authorize endpoint short-circuits
/// to the redirect when its session cookie is valid).
#[cfg(target_os = "macos")]
const DATA_STORE_ID: [u8; 16] = [
    0x6d, 0x69, 0x78, 0x72, 0x2d, 0x72, 0x73, 0x2d,
    0x62, 0x65, 0x61, 0x74, 0x70, 0x6f, 0x72, 0x74,
];

/// Driven by EventLoopProxy so the page-load callback can wake the
/// loop when the OAuth redirect URL is detected.
#[derive(Debug, Clone)]
enum HostEvent {
    CodeCaptured { code: String, state: Option<String> },
    Cancelled,
}

/// Run the WebView OAuth code capture. Doesn't return on macOS
/// (EventLoop::run quirk). The first arg is the `/o/authorize/?...`
/// URL — Beatport will redirect to `dj.beatport.com/home?code=...`
/// once the user signs in (or immediately if cookies are still
/// valid from a previous session).
pub fn run(authorize_url: &str) -> Result<()> {
    let event_loop: EventLoop<HostEvent> = EventLoopBuilder::with_user_event().build();
    let window = WindowBuilder::new()
        .with_title("Sign in to Beatport")
        .with_inner_size(LogicalSize::new(900.0, 750.0))
        .with_resizable(true)
        // Start hidden — the "already signed in via persistent
        // cookies" path completes the redirect within ~500ms, no
        // point flashing a window. A timer below promotes to
        // visible at 1.5s if the redirect hasn't fired yet.
        .with_visible(false)
        .build(&event_loop)?;

    #[cfg(target_os = "macos")]
    let builder = WebViewBuilder::new(&window).with_data_store_identifier(DATA_STORE_ID);
    #[cfg(not(target_os = "macos"))]
    let builder = WebViewBuilder::new(&window);

    let proxy_for_load = event_loop.create_proxy();
    let webview = builder
        .with_url(authorize_url)
        .with_visible(true) // window visibility, not surface
        .with_clipboard(true)
        .with_accept_first_mouse(true)
        .with_on_page_load_handler(move |event, url| {
            if !matches!(event, wry::PageLoadEvent::Started) {
                // We want to catch the redirect BEFORE the page
                // loads (it'll 404 on dj.beatport.com/home anyway —
                // we don't care, we just need the code from the URL).
                return;
            }
            // Match the OAuth redirect — Beatport sends us to
            // dj.beatport.com/home?code=...&state=... after sign-in.
            if !url.contains("dj.beatport.com/home") { return; }
            if let Some((code, state)) = parse_redirect(&url) {
                let _ = proxy_for_load.send_event(HostEvent::CodeCaptured {
                    code, state,
                });
            }
        })
        .build()?;

    // macOS Edit menu so Cmd+V/C/X/A work in the password field.
    #[cfg(target_os = "macos")]
    install_edit_menu();

    // Background timer: if the OAuth redirect hasn't fired in 1.5s,
    // user probably needs to actually sign in — show the window.
    let proxy_for_timer = event_loop.create_proxy();
    let captured = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let captured_for_timer = captured.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        if !captured_for_timer.load(std::sync::atomic::Ordering::Relaxed) {
            // Use a dummy event variant to wake the loop; the loop
            // checks `captured` and shows the window.
            let _ = proxy_for_timer.send_event(HostEvent::Cancelled);
        }
    });

    let _ = webview;
    let window = std::rc::Rc::new(window);
    let captured_for_loop = captured.clone();
    let mut shown_via_timer = false;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(HostEvent::CodeCaptured { code, state }) => {
                captured_for_loop.store(true, std::sync::atomic::Ordering::Relaxed);
                let line = serde_json::to_string(&serde_json::json!({
                    "code": code,
                    "state": state,
                })).unwrap_or_default();
                let mut out = std::io::stdout().lock();
                let _ = writeln!(out, "{line}");
                let _ = out.flush();
                tracing::info!("WebView: OAuth code captured, exiting");
                std::process::exit(0);
            }
            // Timer fired before code captured — promote window.
            // (This event variant doubles as the timer signal — the actual
            // user-cancel path is WindowEvent::CloseRequested.)
            Event::UserEvent(HostEvent::Cancelled)
                if !captured_for_loop.load(std::sync::atomic::Ordering::Relaxed)
                    && !shown_via_timer =>
            {
                window.set_visible(true);
                window.set_focus();
                shown_via_timer = true;
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                // User closed the window without signing in. Print
                // an error JSON so the parent can give a clean
                // message + exit non-zero so the parent knows.
                let line = serde_json::to_string(&serde_json::json!({
                    "error": "user closed sign-in window",
                })).unwrap_or_default();
                let mut out = std::io::stdout().lock();
                let _ = writeln!(out, "{line}");
                let _ = out.flush();
                std::process::exit(1);
            }
            _ => {}
        }
    });
}

/// One-shot client_id discovery from dj.beatport.com.
///
/// Loads dj.beatport.com (the page Beatport itself uses for the DJ
/// web app), polls the rendered DOM and JS bundles for any
/// `client_id=<value>` pattern, and prints the first match on
/// stdout as JSON: `{"client_id":"..."}`. Exits 0 on success, 1
/// on timeout — parent falls back to the hardcoded default.
///
/// Done at runtime so we don't have to ship Beatport's `client_id`
/// embedded in our binary; it's also auto-resilient to rotation.
pub fn run_discover() -> Result<()> {
    #[derive(Debug, Clone)]
    enum DiscoverEvent {
        Found(String),
        Timeout,
    }

    let event_loop: EventLoop<DiscoverEvent> = EventLoopBuilder::with_user_event().build();
    let window = WindowBuilder::new()
        .with_title("Beatport — discovering client_id")
        .with_inner_size(LogicalSize::new(900.0, 750.0))
        .with_visible(false)
        .build(&event_loop)?;

    #[cfg(target_os = "macos")]
    let builder = WebViewBuilder::new(&window).with_data_store_identifier(DATA_STORE_ID);
    #[cfg(not(target_os = "macos"))]
    let builder = WebViewBuilder::new(&window);

    let proxy_for_ipc = event_loop.create_proxy();
    let _webview = builder
        .with_url("https://dj.beatport.com/")
        .with_initialization_script(DISCOVER_SCRIPT)
        .with_visible(true)
        .with_ipc_handler(move |req| {
            let body = req.body();
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(body)
                && let Some(cid) = v.get("client_id").and_then(|x| x.as_str()) {
                    let _ = proxy_for_ipc.send_event(DiscoverEvent::Found(cid.to_string()));
                }
        })
        .build()?;

    // Hard timeout: if the page doesn't surface a client_id within
    // 10s, give up. The parent then surfaces a discovery error.
    let proxy_for_timer = event_loop.create_proxy();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(10));
        let _ = proxy_for_timer.send_event(DiscoverEvent::Timeout);
    });

    event_loop.run(move |event, _, _| {
        match event {
            Event::UserEvent(DiscoverEvent::Found(cid)) => {
                let line = serde_json::to_string(&serde_json::json!({
                    "client_id": cid,
                })).unwrap_or_default();
                let mut out = std::io::stdout().lock();
                let _ = writeln!(out, "{line}");
                let _ = out.flush();
                tracing::info!("WebView: discovered client_id (len={})", cid.len());
                std::process::exit(0);
            }
            Event::UserEvent(DiscoverEvent::Timeout) => {
                let mut out = std::io::stdout().lock();
                let _ = writeln!(out, "{{\"error\":\"client_id discovery timed out\"}}");
                let _ = out.flush();
                std::process::exit(1);
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                std::process::exit(1);
            }
            _ => {}
        }
    });
}

/// JS injected into dj.beatport.com to find the OAuth `client_id`.
/// Beatport's page bundles it into `<a>` href attributes (Sign Up
/// link), into JS modules (referenced as `client_id=...` in the
/// page source), and into URL-encoded form (`client_id%3D...`).
/// The script polls every 200ms searching all those forms.
const DISCOVER_SCRIPT: &str = r#"
(function() {
    if (window.__mixrDiscover) return;
    window.__mixrDiscover = true;
    var found = false;
    function pluck(s) {
        if (typeof s !== 'string') return null;
        // Plain `client_id=VALUE`
        var m = s.match(/client_id=([A-Za-z0-9_-]{20,})/);
        if (m) return m[1];
        // URL-encoded `client_id%3DVALUE`
        m = s.match(/client_id%3D([A-Za-z0-9_-]{20,})/);
        if (m) return m[1];
        return null;
    }
    function search() {
        if (found) return;
        try {
            // outerHTML covers <a href>, <link>, inline scripts
            var html = document.documentElement && document.documentElement.outerHTML || '';
            var cid = pluck(html);
            if (cid) {
                found = true;
                window.ipc.postMessage(JSON.stringify({client_id: cid}));
            }
        } catch (e) {}
    }
    setInterval(search, 200);
    search();
})();
"#;

/// Sign the user out by clearing the WebView's persistent cookie store.
/// Spawned as a child via `mixr --clear-webview-session` so it runs
/// in its own process (the EventLoop::run quirk on macOS).
pub fn run_logout() -> Result<()> {
    #[derive(Debug, Clone, Copy)]
    enum LogoutEvent { Done }

    let event_loop: EventLoop<LogoutEvent> = EventLoopBuilder::with_user_event().build();
    let window = WindowBuilder::new()
        .with_title("Signing out…")
        .with_inner_size(LogicalSize::new(400.0, 300.0))
        .with_visible(false)
        .build(&event_loop)?;

    #[cfg(target_os = "macos")]
    let builder = WebViewBuilder::new(&window).with_data_store_identifier(DATA_STORE_ID);
    #[cfg(not(target_os = "macos"))]
    let builder = WebViewBuilder::new(&window);

    let webview = builder.with_url("about:blank").build()?;
    if let Err(e) = webview.clear_all_browsing_data() {
        tracing::warn!("clear_all_browsing_data failed: {e}");
    }

    let proxy = event_loop.create_proxy();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1000));
        let _ = proxy.send_event(LogoutEvent::Done);
    });
    event_loop.run(move |event, _, _| if let Event::UserEvent(LogoutEvent::Done) = event {
        tracing::info!("WebView cookies cleared");
        std::process::exit(0);
    });
}

/// Parse `code=` and (optional) `state=` query params out of the
/// OAuth redirect URL. Returns None if `code` is missing.
fn parse_redirect(url: &str) -> Option<(String, Option<String>)> {
    let parsed = url::Url::parse(url).ok()?;
    let mut code = None;
    let mut state = None;
    for (k, v) in parsed.query_pairs() {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            _ => {}
        }
    }
    code.map(|c| (c, state))
}

/// Install the standard macOS Edit submenu so Cmd+V / Cmd+C work in
/// the WebView's password and email text fields.
#[cfg(target_os = "macos")]
fn install_edit_menu() {
    use muda::{Menu, Submenu, PredefinedMenuItem};
    let menu = Menu::new();
    let edit = Submenu::new("Edit", true);
    let _ = edit.append(&PredefinedMenuItem::cut(None));
    let _ = edit.append(&PredefinedMenuItem::copy(None));
    let _ = edit.append(&PredefinedMenuItem::paste(None));
    let _ = edit.append(&PredefinedMenuItem::select_all(None));
    let _ = menu.append(&edit);
    menu.init_for_nsapp();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_redirect_extracts_code_and_state() {
        let url = "https://dj.beatport.com/home?code=abc123&state=xyz";
        let (code, state) = parse_redirect(url).unwrap();
        assert_eq!(code, "abc123");
        assert_eq!(state.as_deref(), Some("xyz"));
    }

    #[test]
    fn parse_redirect_missing_code_returns_none() {
        let url = "https://dj.beatport.com/home?state=alone";
        assert!(parse_redirect(url).is_none());
    }

    #[test]
    fn parse_redirect_handles_url_encoded_code() {
        let url = "https://dj.beatport.com/home?code=a%2Bb%2Fc&state=s";
        let (code, _) = parse_redirect(url).unwrap();
        assert_eq!(code, "a+b/c", "url-encoded chars in code must round-trip");
    }
}
