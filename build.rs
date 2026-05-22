// Build script — only does anything when the `rubberband` feature is enabled.
// Links the system librubberband (install with `brew install rubberband`
// on macOS, or `apt install librubberband-dev` on Debian/Ubuntu).

fn main() {
    if std::env::var("CARGO_FEATURE_RUBBERBAND").is_ok() {
        println!("cargo:rustc-link-lib=dylib=rubberband");
        // Common Homebrew lib paths so default `cargo build --features rubberband`
        // works without an extra LDFLAGS dance.
        for p in ["/opt/homebrew/lib", "/usr/local/lib", "/usr/lib"] {
            if std::path::Path::new(p).exists() {
                println!("cargo:rustc-link-search=native={p}");
            }
        }
    }
}
