Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$guiRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $guiRoot
python -m pip install -r requirements.txt
python -m PyInstaller --clean --onefile --windowed --name Codex-Router-Configurator --icon "..\assets\logo.ico" main.py
Copy-Item -Path (Join-Path $guiRoot 'dist' 'Codex-Router-Configurator.exe') -Destination (Join-Path (Split-Path -Parent $guiRoot) 'Codex-Router-Configurator.exe') -Force
Write-Host 'Build complete. EXE copied to project root.'
