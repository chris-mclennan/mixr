//! Platform-specific subprocess helpers.
//!
//! Keeps OS-detection and shell-invocation logic out of the TUI event loop
//! so the recipes are easy to find and extend when adding new package
//! managers or rebuild flags.

/// Install librubberband via the platform's package manager, then rebuild
/// mixr with the `rubberband` cargo feature and ask the app to restart
/// into the new binary. All steps are non-blocking from the caller's point
/// of view (the whole function runs inside the caller's spawned task) and
/// stream progress back through the provided `notify` callback.
///
/// The caller is expected to own the `notify` closure — typically a TUI
/// toast sender — so `platform.rs` has no knowledge of ratatui or tokio.
pub fn install_rubberband(notify: impl Fn(&str) + Send) {
    let (cmd, args, label): (&str, &[&str], &str) = if cfg!(target_os = "macos") {
        ("brew", &["install", "rubberband"], "brew install rubberband")
    } else if cfg!(target_os = "linux") {
        if std::process::Command::new("which").arg("apt").output()
            .map(|o| o.status.success()).unwrap_or(false) {
            ("sudo", &["apt", "install", "-y", "librubberband-dev"], "apt install librubberband-dev")
        } else if std::process::Command::new("which").arg("dnf").output()
            .map(|o| o.status.success()).unwrap_or(false) {
            ("sudo", &["dnf", "install", "-y", "rubberband-devel"], "dnf install rubberband-devel")
        } else if std::process::Command::new("which").arg("pacman").output()
            .map(|o| o.status.success()).unwrap_or(false) {
            ("sudo", &["pacman", "-S", "--noconfirm", "rubberband"], "pacman -S rubberband")
        } else {
            notify("No supported package manager found. Install librubberband-dev manually, then rerun ./run.sh");
            return;
        }
    } else {
        notify("Auto-install not supported on this OS. Install rubberband manually, then rerun ./run.sh");
        return;
    };

    notify(&format!("running: {label}"));
    match std::process::Command::new(cmd).args(args).output() {
        Ok(o) if o.status.success() => notify("Install OK — rebuilding…"),
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            notify(&format!("Install failed: {}", err.lines().next().unwrap_or("unknown")));
            return;
        }
        Err(e) => { notify(&format!("Command not found: {e}")); return; }
    }

    // Rebuild with the feature flag so the restart picks up RubberBand.
    let cwd = std::env::current_dir().unwrap_or_default();
    let build = std::process::Command::new("cargo")
        .args(["build", "--release", "--features", "rubberband"])
        .current_dir(&cwd)
        .output();
    match build {
        Ok(o) if o.status.success() => {
            notify("Built — restarting into rubberband…");
            let _ = std::fs::write(
                dirs::home_dir().unwrap_or_default().join(".mixr/command"),
                b"{\"restart\":1}",
            );
        }
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            notify(&format!("Rebuild failed: {}", err.lines().last().unwrap_or("unknown")));
        }
        Err(e) => notify(&format!("cargo not found: {e}")),
    }
}
