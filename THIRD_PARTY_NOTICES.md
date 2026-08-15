# Third-Party Notices

Codex-Router combines original project material with independent third-party software. Codex-Router is licensed under AGPL-3.0; this does not replace, narrow, or claim ownership of third-party rights.

## Sub2API

- Project: Sub2API
- Upstream: https://github.com/Wei-Shaw/sub2api
- Bundled release: v0.1.170-codex-router.13
- Upstream source commit: https://github.com/Wei-Shaw/sub2api/commit/c043c24774228ba891ddf90d783aa6dc7d0855b5
- Codex-Router compatibility patch series (apply in order): `licenses/sub2api-0.1.170-codex-router.2.patch`, `licenses/sub2api-0.1.170-codex-router.3.patch`, `licenses/sub2api-0.1.170-codex-router.4.patch`, `licenses/sub2api-0.1.170-codex-router.5.patch`, `licenses/sub2api-0.1.170-codex-router.6.patch`, `licenses/sub2api-0.1.170-codex-router.7.patch`, `licenses/sub2api-0.1.170-codex-router.8.patch`, `licenses/sub2api-0.1.170-codex-router.9.patch`, `licenses/sub2api-0.1.170-codex-router.10.patch`, `licenses/sub2api-0.1.170-codex-router.11.patch`, `licenses/sub2api-0.1.170-codex-router.12.patch`, and `licenses/sub2api-0.1.170-codex-router.13.patch`
- License: GNU Lesser General Public License v3.0 or later
- License text: https://github.com/Wei-Shaw/sub2api/blob/c043c24774228ba891ddf90d783aa6dc7d0855b5/LICENSE
- Deployment and operations commitment: https://github.com/Wei-Shaw/sub2api/blob/main/docs/legal/admin-compliance.zh.md
- Upstream copyright and contributor notices remain with the upstream project.

The bundled executable is built from upstream commit `c043c24774228ba891ddf90d783aa6dc7d0855b5` with the documented Codex-Router compatibility patch series applied in order. The series is verified with `git apply --check` against that exact commit. Recipients may exercise the rights granted by Sub2API's upstream license with respect to Sub2API itself. Restrictions in Codex-Router's license apply to original Codex-Router material and the Codex-Router combination/distribution, not to independently obtained upstream Sub2API code.

## PostgreSQL

- Project: PostgreSQL
- Upstream: https://www.postgresql.org/
- Bundled license files: `postgres/pgsql/server_license.txt` and `postgres/pgsql/commandlinetools_3rd_party_licenses.txt`

## Redis and its Windows runtime

- Redis release: 8.10.0
- Redis source: https://github.com/redis/redis/tree/8.10.0
- Redis source archive: https://github.com/redis/redis/archive/refs/tags/8.10.0.tar.gz
- Windows build project: https://github.com/redis-windows/redis-windows/tree/8.10.0
- Windows build source archive: https://github.com/redis-windows/redis-windows/archive/refs/tags/8.10.0.tar.gz
- Windows binary release: https://github.com/redis-windows/redis-windows/releases/tag/8.10.0
- Bundled binary archive SHA-256: `743a39b0a97d0b8ec8355591cd31874dd9da8b0f48a7d6becddacdb082ce6c30`
- Redis and bundled Redis dependency texts: `licenses/Redis-8.10.0-LICENSES.txt`
- MSYS2 runtime, OpenSSL, and GCC runtime texts: `licenses/MSYS2-Runtime-LICENSES.txt`

The Redis Windows archive is an upstream build that includes Redis plus MSYS2 runtime libraries. The accompanying license bundles preserve the license texts from the exact upstream Redis tag and the exact MSYS2 binary packages matched by SHA-256.

## Microsoft Visual C++ Runtime

- Components: `VCRUNTIME140.dll`, `VCRUNTIME140_1.dll`, and `MSVCP140.dll`
- Architecture: x64
- Build source: the official Microsoft Visual Studio `VC/Redist/MSVC/<version>/x64/Microsoft.VC*.CRT` payload, or an explicitly supplied `VC_REDIST_CRT_DIR` copy of that official payload
- Deployment: one app-local copy beside `Codex-Router.exe` and one beside the PostgreSQL executables
- Notice and license references: `licenses/Microsoft-Visual-Cpp-Runtime-NOTICE.txt`

The release builder rejects Windows `System32` and `SysWOW64` as runtime sources and verifies each bundled DLL's x64 PE architecture, Microsoft Authenticode signature, version, and hash. The exact runtime version and per-file hashes are recorded in `dependency-manifest.json` and `release-manifest.json`.

## Rust crates

- Locked dependency graph: `codex-router-gui-rust/Cargo.lock` in the source distribution
- Target used for the portable GUI: `x86_64-pc-windows-msvc`
- Exact crate inventory, crate-supplied notices, and bundled SQLite notice: `licenses/Rust-Crates-LICENSES.txt`
- Canonical SPDX license texts: `licenses/Rust-SPDX-LICENSE-TEXTS.txt`

The Rust crate notice bundle is generated from `cargo metadata --locked` during release construction so its versions remain tied to the executable build rather than a hand-maintained dependency list.
