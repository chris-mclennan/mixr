mod audio;
mod beatport;
mod claude;
mod config;
mod family_offer;
mod favorites;
mod hid;
mod ipc;
mod library_import;
mod local_library;
mod log;
mod midi;
mod platform;
mod session;
mod tui;
mod update_check;
mod usb_libraries;

use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, prelude::*};
use std::io;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::config::AppConfig;
use crate::log::AppLog;
use crate::tui::app::{App, AppAction};

/// Parsed CLI arguments.
pub struct CliArgs {
    pub play: bool,
    pub genre: Option<String>,
    pub shuffle: bool,
    pub quality: Option<String>,
    pub search: Option<String>,
    pub browse: Option<String>,
    pub dashboard: bool,
    pub claude_dj: bool,
    pub claude_dj_prompt: Option<String>,
    pub claude_key: Option<String>,
    pub logout: bool,
    /// Internal-only: spawned by `--logout` to clear the WebView's
    /// persistent cookies. Not user-facing.
    pub clear_webview_session: bool,
    /// Internal-only: spawned by the parent mixr process to capture
    /// an OAuth PKCE auth code by popping a Beatport sign-in window.
    /// Pairs with `--webview-host-url <authorize_url>`. See
    /// beatport::webview_host docs.
    pub webview_host: bool,
    /// The OAuth authorize URL passed to the webview-host child.
    pub webview_host_url: Option<String>,
    /// Internal-only: spawned to discover the `client_id` from
    /// dj.beatport.com at runtime, so we don't ship a hardcoded one.
    pub webview_discover: bool,
    pub help: bool,
    // Standalone commands (don't start TUI)
    pub version: bool,
    pub status: bool,
    pub command: Option<String>,
    pub export: bool,
    pub favorites_list: bool,
    /// Native/integrated mode under tmnl — connect to the given UDS
    /// socket as a blit client (TestBackend + binary cell frames).
    /// When set, mixr skips the crossterm terminal setup entirely.
    pub blit: Option<String>,
}

/// Every flag mixr recognises — `unknown_flag` checks args against
/// this list and returns the first non-matching token. Keep in sync
/// with `CliArgs::parse` and `print_help` below.
const KNOWN_FLAGS: &[&str] = &[
    "--play",
    "--genre",
    "--shuffle",
    "--quality",
    "--search",
    "--browse",
    "--dashboard",
    "--claude-dj",
    "--claude-key",
    "--logout",
    "--clear-webview-session",
    "--webview-host",
    "--webview-discover",
    "--help",
    "-h",
    "--version",
    "-V",
    "--status",
    "--command",
    "--export",
    "--favorites",
    "--blit",
];

/// Walk `args[1..]` and return the first token that starts with `--`
/// (or `-V` / `-h` style short flag) that isn't in `KNOWN_FLAGS`.
/// `None` when every flag is recognised. Positional args (the URL
/// after `--webview-host`, the search term, etc.) don't start with
/// `-` so they don't trigger false positives.
fn unknown_flag(args: &[String]) -> Option<String> {
    args.iter()
        .skip(1)
        .find(|a| {
            (a.starts_with("--") || (a.len() == 2 && a.starts_with('-')))
                && !KNOWN_FLAGS.contains(&a.as_str())
        })
        .cloned()
}

impl CliArgs {
    fn parse(args: &[String]) -> Self {
        let flag = |name: &str| args.contains(&name.to_string());
        let value = |name: &str| -> Option<String> {
            args.iter()
                .position(|a| a == name)
                .and_then(|i| args.get(i + 1))
                .filter(|v| !v.starts_with("--"))
                .cloned()
        };

        Self {
            play: flag("--play"),
            genre: value("--genre").or_else(|| value("--play").filter(|v| !v.is_empty())),
            shuffle: flag("--shuffle"),
            quality: value("--quality"),
            search: value("--search"),
            browse: value("--browse"),
            dashboard: flag("--dashboard"),
            claude_dj: flag("--claude-dj"),
            claude_dj_prompt: value("--claude-dj"),
            claude_key: value("--claude-key"),
            logout: flag("--logout"),
            clear_webview_session: flag("--clear-webview-session"),
            webview_host: flag("--webview-host"),
            // Positional URL after --webview-host. Parsed by hand
            // because the `value` helper above only catches `--flag value`
            // form, not direct positional after a known flag.
            webview_host_url: args
                .iter()
                .position(|a| a == "--webview-host")
                .and_then(|i| args.get(i + 1))
                .filter(|v| !v.starts_with("--"))
                .cloned(),
            webview_discover: flag("--webview-discover"),
            help: flag("--help") || flag("-h"),
            version: flag("--version") || flag("-V"),
            status: flag("--status"),
            command: value("--command"),
            export: flag("--export"),
            favorites_list: flag("--favorites"),
            blit: value("--blit"),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let raw_args: Vec<String> = std::env::args().collect();

    // Reject unknown flags BEFORE we boot the TUI. Without this,
    // a typo like `--cheeck` falls through to the audio engine
    // which then `?`-bubbles a "no default device" anyhow trace
    // through main — a 40-line backtrace for what was just a typo.
    // (2026-06-08 hunt H3.)
    if let Some(bad) = unknown_flag(&raw_args) {
        eprintln!("mixr: unknown flag: {bad}");
        eprintln!("Run `mixr --help` for usage.");
        std::process::exit(2);
    }

    let args = CliArgs::parse(&raw_args);

    if args.help {
        print_help();
        return Ok(());
    }

    if args.version {
        println!("mixr-rs 0.1.0");
        return Ok(());
    }

    // Standalone commands — don't start TUI
    if args.status {
        let path = dirs::home_dir().unwrap_or_default().join(".mixr/quick.txt");
        match std::fs::read_to_string(&path) {
            Ok(content) => println!("{content}"),
            Err(_) => {
                // Try status.json
                let path2 = dirs::home_dir()
                    .unwrap_or_default()
                    .join(".mixr/status.json");
                match std::fs::read_to_string(&path2) {
                    Ok(content) => println!("{content}"),
                    Err(_) => println!("No status available (is mixr running?)"),
                }
            }
        }
        return Ok(());
    }

    if let Some(ref cmd) = args.command {
        let path = dirs::home_dir().unwrap_or_default().join(".mixr/command");
        // Three accepted forms:
        //   1. Raw JSON:     mixr --command '{"skip":1}'
        //   2. Bare keyword: mixr --command skip          → {"skip":1}
        //   3. Key + value:  mixr --command "queue 12345" → {"queue":12345}
        //                    mixr --command "vol 0.8"     → {"vol":0.8}
        //                    mixr --command "tx echoout"  → {"tx":"echoout"}
        // Same parsing as the in-app `:` command-prompt — value type
        // is inferred (int/float/bool/string).
        let json = if cmd.trim_start().starts_with('{') {
            cmd.clone()
        } else {
            ipc::shorthand_to_json(cmd)
        };
        if serde_json::from_str::<serde_json::Value>(&json).is_err() {
            eprintln!("Bad command: {cmd}");
            return Ok(());
        }
        std::fs::write(&path, &json)?;
        println!("Command sent: {json}");
        return Ok(());
    }

    if args.export {
        let path = dirs::home_dir().unwrap_or_default().join(".mixr/command");
        std::fs::write(&path, r#"{"export":1}"#)?;
        println!("Export command sent (history will be saved to ~/.mixr/)");
        return Ok(());
    }

    if args.favorites_list {
        let favs = favorites::FavoritesDB::load();
        let tracks = favs.all_tracks();
        if tracks.is_empty() {
            println!("No favorites.");
        } else {
            for (i, t) in tracks.iter().enumerate() {
                let bpm = t.bpm.map(|b| format!("{:.0}", b)).unwrap_or("?".into());
                let key = t.key.as_deref().unwrap_or("?");
                println!(
                    "{:>3}. {} - {}  {bpm} BPM / {key}",
                    i + 1,
                    t.artist_name(),
                    t.full_title()
                );
            }
            println!("\n{} favorites", tracks.len());
        }
        return Ok(());
    }

    let _guard = AppLog::init()?;
    let mut config = AppConfig::load();

    // Hardware controller listeners — kept on Arc handles so the TUI
    // can read the latest MIDI/HID event for the learn screen and
    // mutate the binding map. Both listeners run for the lifetime of
    // the process; both fail gracefully (no MIDI port = log + idle,
    // no HID device = log + idle).
    let midi_state = midi::spawn_listener();
    let _hid_state = hid::spawn_listener();

    // --webview-host <authorize_url>: spawned by the parent for one-shot
    // OAuth PKCE code capture. Pops a Beatport sign-in window, watches
    // for the redirect to dj.beatport.com/home?code=..., prints the
    // captured code on stdout, exits. Doesn't return on macOS.
    if args.webview_host {
        let url = args
            .webview_host_url
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("--webview-host needs an authorize URL"))?;
        beatport::webview_host::run(url)?;
        return Ok(());
    }

    // --webview-discover: one-shot subprocess that loads dj.beatport.com
    // and scrapes the OAuth client_id from the page. See webview_host
    // docs.
    if args.webview_discover {
        beatport::webview_host::run_discover()?;
        return Ok(());
    }

    // --clear-webview-session: spawned by --logout to clear the
    // WebView's persistent data store. Doesn't return on macOS.
    if args.clear_webview_session {
        beatport::webview_host::run_logout()?;
        return Ok(());
    }

    // --logout: spawn a child to clear the WebView's persistent
    // cookies (the only place auth lives now), then exit. Next
    // launch will pop the sign-in window again.
    if args.logout {
        println!("Clearing browser session...");
        let exe = std::env::current_exe()?;
        let _ = std::process::Command::new(exe)
            .arg("--clear-webview-session")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        println!("Signed out.");
        return Ok(());
    }

    // --claude-key: store API key and exit
    if let Some(key) = &args.claude_key {
        let dir = dirs::home_dir().unwrap_or_default().join(".mixr");
        std::fs::create_dir_all(&dir)?;
        let key_path = dir.join("claude_key");
        std::fs::write(&key_path, key)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).ok();
        }
        config.claude_dj_enabled = true;
        config.save();
        println!("Claude DJ API key saved.");
        return Ok(());
    }

    // --quality: override audio quality
    if let Some(q) = &args.quality {
        match q.to_lowercase().as_str() {
            // FLAC isn't reachable on main's auth scope; coerce to 256k.
            "flac" | "lossless" => config.audio_quality = config::AudioQuality::High,
            "256k" | "256" | "high" => config.audio_quality = config::AudioQuality::High,
            "128k" | "128" | "standard" => config.audio_quality = config::AudioQuality::Standard,
            _ => {}
        }
    }

    // OAuth PKCE flow. If we have a stored access_token that hasn't
    // expired, use it. If we have a refresh_token, try refreshing.
    // Otherwise pop the Beatport sign-in WebView, capture the OAuth
    // code from the redirect, exchange for tokens.
    let auth = match acquire_auth().await {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Beatport sign-in failed: {e}");
            return Err(e);
        }
    };
    println!("Signed in.");

    tracing::info!(
        "Config: bpmMode={:?}, quality={:?}, crossfadeBars={}",
        config.bpm_mode,
        config.audio_quality,
        config.crossfade_bars
    );

    // Native/integrated mode under tmnl: render into a TestBackend and
    // ship binary cell frames over the `--blit` Unix socket instead of
    // driving a crossterm terminal — no alt-screen / raw-mode setup.
    if let Some(socket) = args.blit.clone() {
        let (action_tx, action_rx) = mpsc::unbounded_channel::<AppAction>();
        let mut app = App::new(config, action_tx.clone(), auth).await?;
        app.midi = Some(midi_state);
        app.apply_cli_args(&args).await;
        // Tell the render layer it's painting into a tmnl native pane
        // so it leaves a 1-cell horizontal margin + 2 reserved rows
        // at the bottom (cmdline + gutter).
        app.native_mode = true;
        return crate::tui::blit::run(app, action_rx, std::path::Path::new(&socket)).await;
    }

    // Setup terminal — mouse capture is on so the dashboard can be
    // driven with clicks/drags + scroll wheel. macOS Terminal,
    // iTerm2, Linux gnome-terminal, and Windows Terminal all speak
    // xterm-style mouse codes that crossterm decodes for us.
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        // OSC 0/2 — sets the terminal window/tab title to "mixr" so any
        // host terminal (Apple Terminal, iTerm2, tmnl, mnml's pty pane,
        // …) shows "mixr" in its tab strip. Harmless on terminals that
        // don't honor OSC sequences.
        crossterm::terminal::SetTitle("mixr"),
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal, config, auth, args, midi_state).await;

    // Restore terminal — disable mouse before leaving alt-screen so
    // the parent shell doesn't get spammed with mouse codes.
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    if let Err(e) = result {
        eprintln!("Error: {e}");
    }

    Ok(())
}

/// Acquire a Beatport access_token via OAuth PKCE. Tries (in order):
/// 1. Stored access_token still within its TTL
/// 2. Refresh existing refresh_token (if cached client_id present)
/// 3. Pop the WebView for fresh sign-in + code exchange (after a
///    `client_id` discovery pass if not cached)
async fn acquire_auth() -> Result<beatport::auth::StoredAuth> {
    let stored = beatport::auth::StoredAuth::load();
    if stored.looks_live() {
        tracing::info!("Using cached access_token");
        return Ok(stored);
    }

    // We need a client_id for any /token/ call. Use the cached one
    // first; otherwise discover it from dj.beatport.com. If discovery
    // fails (network down, page format changed, …) surface the error
    // rather than fall back to a bundled credential.
    let client_id = match stored.client_id.clone() {
        Some(cid) => cid,
        None => {
            println!("Discovering Beatport client_id…");
            tokio::task::spawn_blocking(beatport::webview_client::discover_client_id)
                .await
                .map_err(|e| anyhow::anyhow!("discover task panicked: {e}"))
                .and_then(|r| r)?
        }
    };

    if let Some(rt) = stored.refresh_token.as_deref() {
        tracing::info!("access_token expired — refreshing");
        if let Ok(refreshed) = beatport::auth::refresh(rt, &client_id).await {
            return Ok(refreshed);
        }
        // Refresh failed (token revoked, server change, client_id
        // rotated, …) — fall through to fresh sign-in.
    }

    println!("Opening Beatport sign-in window…");
    let pkce = beatport::auth::PkcePair::generate();
    let authorize_url = pkce.authorize_url(&client_id);

    let captured = tokio::task::spawn_blocking(move || {
        beatport::webview_client::capture_oauth_code(&authorize_url)
    })
    .await
    .map_err(|e| anyhow::anyhow!("WebView capture task panicked: {e}"))??;

    let auth = beatport::auth::exchange_code(&captured.code, &pkce.verifier, &client_id).await?;
    Ok(auth)
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    config: AppConfig,
    auth: beatport::auth::StoredAuth,
    cli: CliArgs,
    midi_state: std::sync::Arc<std::sync::Mutex<midi::ListenerState>>,
) -> Result<()> {
    let (action_tx, mut action_rx) = mpsc::unbounded_channel::<AppAction>();
    let mut app = App::new(config, action_tx.clone(), auth).await?;
    app.midi = Some(midi_state);

    // Background "is there a newer release?" probe — fires a
    // one-shot toast on the first tick after the GET resolves with
    // a newer GitHub tag than CARGO_PKG_VERSION.
    app.update_check = Some(crate::update_check::UpdateCheck::spawn());

    // First-launch family offer — one-shot toast(s) for any missing
    // mnml / tmnl. Marker at ~/.config/mixr/.family-offer-shown
    // suppresses re-fires.
    if let Some(offer) = crate::family_offer::FamilyOffer::maybe_new() {
        for line in offer.hint_lines() {
            app.toast.show(&line, 12.0);
        }
        offer.mark_shown();
    }

    // Apply CLI startup actions
    app.apply_cli_args(&cli).await;

    loop {
        terminal.draw(|frame| {
            app.render(frame);
        })?;

        if event::poll(Duration::from_millis(16))? {
            match event::read()? {
                Event::Key(key) => {
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        break;
                    }
                    app.handle_key(key);
                }
                Event::Mouse(m) => app.handle_mouse(m),
                _ => {}
            }
        }

        while let Ok(action) = action_rx.try_recv() {
            app.handle_action(action).await;
        }

        app.tick().await;
    }

    Ok(())
}

fn print_help() {
    println!(
        r#"mixr — Beatport terminal DJ

Usage:
  mixr                              Interactive TUI
  mixr --play                       Queue random favorite genre chart
  mixr --play "Melodic House & Techno" --shuffle    Queue specific genre + shuffle
  mixr --search "ARTBAT"          Jump to search results
  mixr --browse "Genres/Melodic House & Techno"     Navigate to path
  mixr --claude-dj "peak hour"      Claude DJ runs the set
  mixr --claude-key KEY             Store Anthropic API key
  mixr --logout                     Clear credentials

Options:
  --play               Queue top chart (random favorite genre)
  --play "Genre"       Queue specific genre chart
  --genre "name"       Set genre (works with --play and --claude-dj)
  --shuffle            Smart shuffle queue (BPM + key aware)
  --quality flac|256k|128k  Set audio quality
  --search "query"     Jump to search results
  --browse "path"      Navigate path (e.g. "Genres/Techno/Top 100")
  --dashboard          Start on dashboard view
  --claude-dj "prompt" Claude DJ runs the set (optional: style/BPM/direction)
  --claude-key KEY     Set Anthropic API key for Claude DJ
  --logout             Clear stored credentials
  --help, -h           Show this help
  --version, -V        Show version

Standalone (don't start TUI — can be used while app is running):
  --status             Print current playback status
  --command '{{"skip":1}}'   Send IPC command to running instance (raw JSON)
  --command skip            Shorthand: {{"skip":1}}
  --command "queue 12345"   Shorthand: {{"queue":12345}} (key + value)
  --command "vol 0.8"       Shorthand: {{"vol":0.8}}
  --command "tx echoout"    Shorthand: {{"tx":"echoout"}}
  --export             Tell running instance to export history
  --favorites          List all favorited tracks

Browsing:
  ↑↓         Navigate        Enter/→  Select / drill into
  ←          Back            Esc      Back (quit at root)
  /  s       Search          Ctrl+F   Filter current list
  b          Jump to browse  L        Load more (pagination)
  ←→         Column nav (on track lists: title/artist/label/genre/date)

Playback:
  Space      Preview track   Enter    Queue track
  p          Pause / play    n        Skip track
  t          Teleport        m        Mix now (force crossfade)
  < >        Jump ±N bars    [ ]      Nudge incoming
  S          Split cue       M        Metronome
  a          Queue all       x        Smart shuffle (BPM+key)

Queue & Tracks:
  q          View queue      X        Clear queue
  h          History         e        Export history
  {{ }}        Grab / drop (reorder queue)
  f  *       Toggle favorite r        Sync favorites audio
  o          Open in browser +        Add to playlist
  w          Follow artist/label      y  Copy screen to clipboard

Views:
  d          Dashboard       v        Compact / full view
  w          Waveform mode   c  C     Toggle Claude DJ
  ,          Settings        ?        Help
  Tab        Dashboard focus (Controller → Queue → History → Browse)
  Ctrl+C     Quit

Files:
  ~/.mixr/config.json       Settings
  ~/.mixr/auth.json         Credentials
  ~/.mixr/mixr.log          Engine log
  ~/.mixr/favorites.json    Favorited tracks (metadata only)
  ~/.mixr/status.json       Live status (auto-updated)
  ~/.mixr/screen.txt        Screen dump
  ~/.mixr/quick.txt         Quick status
  ~/.mixr/command            Remote control (JSON)"#
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        std::iter::once("mixr")
            .chain(v.iter().copied())
            .map(String::from)
            .collect()
    }

    #[test]
    fn unknown_flag_rejects_typo() {
        assert_eq!(
            unknown_flag(&args(&["--cheeck"])).as_deref(),
            Some("--cheeck")
        );
        assert_eq!(
            unknown_flag(&args(&["--check"])).as_deref(),
            Some("--check")
        );
        assert_eq!(unknown_flag(&args(&["-x"])).as_deref(), Some("-x"));
    }

    #[test]
    fn unknown_flag_accepts_known() {
        assert_eq!(unknown_flag(&args(&["--version"])), None);
        assert_eq!(unknown_flag(&args(&["-V"])), None);
        assert_eq!(unknown_flag(&args(&["--help"])), None);
        assert_eq!(unknown_flag(&args(&["-h"])), None);
        assert_eq!(unknown_flag(&args(&["--status"])), None);
        assert_eq!(
            unknown_flag(&args(&["--genre", "house", "--shuffle"])),
            None
        );
    }

    #[test]
    fn unknown_flag_ignores_positionals() {
        // `--webview-host` takes a positional URL after it. The URL
        // doesn't start with `-` so unknown_flag must not flag it.
        assert_eq!(
            unknown_flag(&args(&["--webview-host", "https://soundcloud.com"])),
            None
        );
        // Bare positional (e.g. search term)
        assert_eq!(unknown_flag(&args(&["techno"])), None);
    }
}
