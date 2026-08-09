# Repository Guide

- This project combines a Rust `eframe` GUI with Windows PowerShell runtime, routing, build, and release scripts.
- Usage querying, provider normalization, bounded concurrency, and last-good caching live in `codex-router-gui-rust/src/logic/usage.rs`; release packages must not contain `Get-UsageMonitor.ps1`.
- Run focused usage checks from `codex-router-gui-rust` with `cargo test --locked logic::usage::tests`.
- Validate Rust changes from `codex-router-gui-rust` with `cargo fmt --all`, `cargo check --locked`, `cargo test --locked`, and `cargo clippy --locked --all-targets -- -D warnings`.
- Never stop or restart the Router/Sub2API instance that carries the active Codex session unless the user explicitly authorizes it. Build into new output directories instead of overwriting a running stage.
- Never print, persist in fixtures, or package API keys, OAuth tokens, cookies, AK/SK credentials, account emails, or raw authenticated responses.
- Do not commit, push, publish releases, or write to GitHub without explicit user authorization.
