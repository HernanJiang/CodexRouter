@echo off
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
cd /d "%~dp0"
cargo clean
cargo build --release
copy /Y target\release\codex-router-configurator.exe ..\Codex-Router-Configurator.exe
