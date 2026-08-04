# CC Switch integration archive

This directory preserves the complete CC Switch implementation removed from
the 0.4.4 product surface. It is source-only and is not copied into portable
releases.

Contents:

- `rust-source/`: the 0.4.3 Rust GUI, configuration, profile, and sync logic.
- `scripts/`: database synchronization, offline update, and related tests.
- `configurator/`: the retired browser configurator that exposed CC Switch.

The active product keeps Router-managed local profiles, rollback points,
per-profile credentials, and shared Codex account/history behavior. Reintroduce
CC Switch only after its database and settings contracts have a stable,
versioned integration API.
