@echo off
setlocal
cd /d "%~dp0"
echo.
echo Codex-Router fresh-environment test
echo - Isolated user data: %~dp0FreshUserData
echo - Isolated Codex home: %~dp0FreshUserData\codex-home
echo - Isolated ports: 28080 / 28081
echo - Does not stop or overwrite the production Release\ instance on 18080
echo - First launch opens the welcome wizard
echo.
echo After the window opens: accept terms, add one ChatGPT subscription or API
echo channel, Save and apply, then restart Codex and run the 8-item checklist.
echo.
set CODEX_ROUTER_PORTABLE_STATE=
set CODEX_ROUTER_FORCE_WELCOME=1
set CODEX_ROUTER_USER_DATA_ROOT=%~dp0FreshUserData
set CODEX_HOME=%~dp0FreshUserData\codex-home
set CODEX_ROUTER_HOST_PORT=28080
set CODEX_ROUTER_CLI_PORT=28081
if not exist "%CODEX_ROUTER_USER_DATA_ROOT%" mkdir "%CODEX_ROUTER_USER_DATA_ROOT%"
if not exist "%CODEX_HOME%" mkdir "%CODEX_HOME%"
"%~dp0Codex-Router.exe"
