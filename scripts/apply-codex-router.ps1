Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# Stable entry point shared by the desktop app and manual deployments.
& (Join-Path $PSScriptRoot 'Apply-Router.ps1') @args
