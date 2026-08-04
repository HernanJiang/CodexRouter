@echo off
cd /d "%~dp0"
cargo clean
cargo build --release
copy /Y target\release\codex-router.exe ..\Codex-Router.exe
