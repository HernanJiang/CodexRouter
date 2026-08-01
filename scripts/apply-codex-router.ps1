Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Backward-compatible entry point. The Rust GUI calls the same implementation
# directly, so manual users and older shortcuts receive identical behavior.
& (Join-Path $PSScriptRoot 'Apply-Configurator.ps1') @args
