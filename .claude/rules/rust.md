# Rust Rules

- Rust 2024 edition — use latest idioms
- `cargo clippy` must pass with no warnings before committing
- Error handling: `thiserror` for library errors, `anyhow` for application errors
- No `unwrap()` in library code — use `?` or explicit error handling. `unwrap()` OK in tests and infallible cases.
- Prefer `Arc<Mutex<T>>` over `unsafe` for shared state between audio thread and main thread
- Audio callback: no heap allocation, no I/O, no locks held longer than a buffer fill
- Use `tracing` macros, not `println!` or `eprintln!`
- Module structure: one file per logical unit, `mod.rs` for re-exports
- Tests go in `#[cfg(test)] mod tests` at the bottom of each file, or in `tests/` for integration tests
- Run `cargo test` before committing
