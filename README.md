<p align="center">
  <a href="README.zh-CN.md"><strong>中文 README</strong></a>
</p>

<p align="center">
  <img src="assets/release/codex-router-logo.png" alt="Codex-Router logo" width="128">
</p>

<h1 align="center">Codex-Router</h1>

<p align="center"><strong>One router for every model, subscription, and API channel.</strong></p>

<p align="center">
  <img src="assets/release/codex-router-banner.png" alt="Codex-Router banner" width="100%">
</p>

<p align="center">
  <img src="https://img.shields.io/badge/version-1.6.11-0969da" alt="Version 1.6.11">
  <img src="https://img.shields.io/badge/platform-Windows%2010%20%2F%2011-0078d4" alt="Windows 10/11">
  <img src="https://img.shields.io/badge/architecture-x64-555555" alt="x64">
  <img src="https://img.shields.io/badge/runtime-portable-2ea44f" alt="Portable runtime">
</p>

<p align="center">
  <a href="#overview">Overview</a> ·
  <a href="#model-routing">Model routing</a> ·
  <a href="#usage-monitoring">Usage monitoring</a> ·
  <a href="#download-and-first-run">Download</a> ·
  <a href="#security-and-terms">Security</a>
</p>

Codex-Router keeps the original Codex workflow while adding a unified model menu for multiple providers, OAuth subscription accounts, and third-party API channels. With automatic continuity enabled, matching OAuth and API channels share one public model and subscription quota is preferred. With it disabled, the API and OAuth routes remain separate choices while keeping the same model display name, so the user can choose the quota source directly.

## Overview

<p align="center">
  <img src="assets/release/promotion.png" alt="Codex-Router overview" width="100%">
</p>

Codex-Router is a Windows desktop router built around Sub2API and a Rust desktop console. PostgreSQL, Redis, Sub2API, the router runtime, and the required app-local VC++ runtime are included in the portable package. Services listen on the local loopback interface.

### Why it is useful

- Keep working in Codex while switching between providers and accounts from one model menu.
- Prefer subscription capacity and continue through an API fallback when a subscription is limited or unavailable.
- Observe OAuth accounts, coding plans, token usage, windows, balances, reset times, and API usage from one aggregated dashboard.
- Keep per-model context, compaction, multimodal, and reasoning settings independent.
- Run the router from the tray with very low background overhead.

## Model Routing

### One menu, many channels

Codex-Router can merge configured OAuth and API channels into the model list that Codex sees. With automatic continuity enabled, the same public model ID appears once while backend routing priorities remain independent. With it disabled, the API and OAuth routes expose stable distinct IDs but the same display name, allowing explicit quota selection from the model menu without adding an `(OAuth)` suffix.

<p align="center">
  <img src="assets/release/screenshot-codex.png" alt="Switching models from the Codex model menu" width="900">
</p>

### Seamless context switching

Switch models directly from the Codex model menu and continue in the same context window. The conversation, working directory, and task state stay in Codex while Router changes the selected backend channel. This makes it practical to move from a subscription model to an API channel, or between providers, without opening a new workflow.

### OAuth and API hybrid routing

- Supports the Sub2API login entries for OpenAI/ChatGPT, Anthropic/Claude, Google Gemini, Google Antigravity, and xAI/Grok.
- Shows the account plan, status, available capacity, reset information, and models discovered by the upstream platform.
- Every manual or scheduled self-check refreshes each OAuth account's live available-model list. Only models declared by that account are shown, discovery never imports them automatically, and a model is added only after the user clicks its `+ model` button.
- An added OAuth model can be removed from the current profile from its right-click menu. Save & apply respects the deletion and does not restore it from discovery.
- Stores OAuth account selection independently for each routing profile. Only models the user added and enabled participate in that profile.
- With automatic continuity enabled, prefers subscription capacity for a matching model and falls back to a lower-priority API channel when the subscription is exhausted or unhealthy. With it disabled, no automatic handoff occurs and the selected model entry determines the quota source.
- Uses the upstream reset time when available. When no reset time is exposed, Router performs a low-frequency recovery probe and automatically returns to the subscription after recovery.
- Keeps OAuth tokens under Sub2API management. Tokens are not written to the Codex-Router configuration file and are not offered as plaintext exports.

### Model-aware controls

Each model can have its own default context window, automatic compaction threshold, image input capability, and reasoning strength. Mainstream model families have adapted reasoning menus, context defaults, compaction ratios, and multimodal settings so the recommended values are ready to use. Models without a built-in adaptation still keep an editable manual settings window instead of being locked to a generic parameter.

<p align="center">
  <img src="assets/release/feature-thinking-intensity.png" alt="Model-aware reasoning strength menu" width="620">
</p>

The model catalog also deduplicates public IDs while preserving backend channel redundancy. GPT-5.6 Sol/Terra, Luna, Claude, Gemini, Grok, Kimi, GLM, and other configured model families can keep provider-specific reasoning and multimodal behavior.

## Usage Monitoring

### A dedicated aggregated monitoring view

Usage monitoring is a first-class part of Codex-Router, not a small detail of OAuth login. It aggregates the real-time state of multiple OAuth accounts, API channels, and coding plans in one dashboard, including:

- subscription windows and reset countdowns;
- five-hour, daily, weekly, and monthly coding-plan limits;
- Volcengine Ark Coding Plan weekly and monthly capacity when control-plane credentials are configured;
- Kimi, Grok, Z.ai/GLM, MiniMax, MiMo, OpenRouter, DeepSeek, ZenMux, and other supported channel usage;
- token totals, requests, model-level usage, cost, balances, and provider error state;
- last-good data with bounded cache fallback when a provider temporarily rejects or delays a usage query.

Usage refresh now runs independent provider tasks with bounded concurrency and per-task deadlines. A slow Grok, Kimi, or API channel can time out independently while other cards continue to return; compatible quota payloads are normalized across nested, ratio-based, and provider-specific response shapes.

The view keeps OAuth quota cards and API usage cards visible together, packs cards dynamically into independent columns, and avoids large blank areas when accounts have different numbers of quota windows.

## Tray Performance

Codex-Router can start with Windows in a lightweight tray mode without launching an additional daemon. Tray mode pauses log following, UI refresh, and high-frequency usage updates. It retains one native health check every 60 seconds, local-service recovery after consecutive failures, and the unified self-check every 10 minutes.

The 1.6.11 runtime retains the memory and background-work optimizations. Idle tray CPU, disk, and network activity are designed to be effectively negligible; the screenshot below shows the router process at 0% CPU and 0 Mbps network activity in the tested idle state.

<p align="center">
  <img src="assets/release/usage-performance.png" alt="Codex-Router idle resource usage" width="900">
</p>

### What is new in 1.6.11

- The OAuth account page now reads accounts and live model catalogs through the native Rust Sub2API admin client. The PowerShell-free release no longer depends on the removed `Get-OAuthAccounts.ps1`, so working Grok, Antigravity, and ChatGPT accounts are no longer shown as having no declared models or an unreadable catalog.
- Both live Sub2API response shapes are supported: account catalogs returned as `data` model objects and upstream synchronization returned as `data.models` identifiers. ChatGPT falls back to its stable catalog when synchronization is unsupported; Grok and Antigravity refresh upstream and then use the current account catalog.
- A transient refresh keeps the last successful model list with a retry notice. Manual refresh and the ten-minute self-check update only the selectable catalog and never bulk-add subscription models to the active profile.
- A fresh installation opens on page one of the five-stage setup guide. Users with a completed configuration still go directly to the dashboard.
- Google Antigravity OAuth has dedicated compatibility support, allowing eligible Google AI Pro accounts to use their Antigravity subscription quota. Available models and quota remain subject to the account's live declaration and Google's upstream status.

### What changed in 1.6.10

- DeepSeek, Kimi, Claude, Gemini, Grok, and other third-party models remain directly usable while Codex keeps its ChatGPT sign-in. If another application overwrites the Codex configuration, startup, manual refresh, and the ten-minute self-check restore the local Router provider, bearer, and model catalog while preserving the selected model, ChatGPT authentication, and user settings without closing or restarting Codex.
- OAuth account cards show each account's live declared models. A failed refresh retains the last successful catalog with a retry notice, does not make the account look empty, and never bulk-adds subscription models to the active profile.
- The model editor adds **Add subscription account model**, which opens the OAuth account page and starts a self-check refresh. The actions are ordered **Cancel**, **Add subscription account model**, and **Save model**; subscription models are still added only one at a time by the user.
- Sparse Antigravity account mappings now normalize Gemini 3.1 Pro compatibility names to the executable `gemini-pro-agent`, fixing `Upstream request failed` in Codex while preserving explicit custom mappings.
- `gemini-3.7-flash` and its Low, Medium, and High execution tiers use a non-zero Flash accounting fallback when dynamic pricing is unavailable, so successful responses no longer leave `pricing not found` errors.

### What is new in 1.6.9

- Antigravity Responses requests that combine web search with function tools no longer fail upstream before output begins. Because the Antigravity `cloudcode-pa` endpoint rejects that combination, the Antigravity boundary keeps executable function tools and removes only server-side search; Google AI Studio requests keep both tools unchanged.
- The public `gemini-3.7-flash` choice maps to the live `gemini-3.7-flash-medium` tier, matching Antigravity's default Medium setting. Live High, Medium, and Low account entries are normalized into this single user-facing model instead of being mistaken for unrelated models.
- Antigravity quota entries that share one upstream usage/reset signature are shown once per real quota pool; internal `chat_*` and `tab_*` entries are hidden. API and coding-plan models that use the same provider endpoint and the same real Key also share one quota display, including Kimi for Coding and Kimi K3.
- Grok OAuth routing recognizes the live `grok-4.6` subscription model in both the default account mapping and scheduler.
- OAuth model refresh uses each account's live upstream synchronization endpoint. Manual refresh and the ten-minute self-check update only the selectable catalog; models are still added to a profile only by an explicit user action.

### What changed in 1.6.8

- Antigravity models such as Gemini 3.1 Pro High and Claude Fable 5 remain routable when one duplicate OAuth account is isolated and another selected account is healthy. The healthy account now takes over the shared model routes instead of leaving them without an eligible backend.
- Every manual or ten-minute self-check refreshes each OAuth account's live model catalog. Static provider suggestions and the manual OAuth model bypass are removed, so the UI cannot advertise a model the account did not declare and newly available models appear without being auto-added.
- Added OAuth models support account-scoped removal from a right-click menu. Removing one account's copy preserves the same model on other accounts and clears its fallback selection only after the last OAuth copy is removed.
- In the clay theme, the Profile OAuth button now uses the same coffee color as Save & apply instead of an unrelated green.

### What changed in 1.6.7

- Healthy Grok and Gemini / Antigravity usage results now clear stale account-level `request_failure` probe errors. Authentication, permission, rate-limit, disabled-account, and exhausted-quota states remain visible and actionable.
- Account-level transient probe failures no longer claim that the complete local OAuth service is unavailable. The message identifies the affected upstream account and explains that self-check will retry automatically.
- The dashboard overview card now aligns with the combined model and activity-log column. Model cards use tighter spacing between the model name and route badges, showing three complete entries at the default window size, while proxy, update, language, and theme controls return to white surfaces.

### What changed in 1.6.6

- Upgrades from 1.6.4 or 1.6.5 remove only the legacy bulk OAuth catalog entries that were never user-selected. API channels, manually added OAuth models, renamed models, and small existing selections are preserved. OAuth refresh and Save & apply never restore deleted catalog entries.
- Dashboard navigation actions are anchored to the right. The top bar uses a deeper mist-blue surface, the language selector has two non-overlapping equal segments, and the five-stage quick-start slider uses the available space beside the proxy controls.
- Router cards are denser and the model list shows about 2.5 entries at the default window size. The activity log keeps a complete fixed area above the bottom edge instead of overlapping the footer.
- Antigravity models that report the same server-side quota signature are displayed as one shared quota pool. Models with different usage or reset windows remain separate.

### What changed in 1.6.5

- Usage self-checks no longer read or import OAuth model catalogs. Refreshing OAuth displays the account's available models, but each model is added only after the user clicks its `+ model` button. Save & apply respects deletions instead of restoring dozens of subscription models.
- The default window now opens at the compact console size. A five-stage quick-start progress slider replaces the old `Console 5/5` badge.
- Live usage, provider setup, profile switching, and profile OAuth stay beside the console title. Save & apply and `+ Add model` now sit in the router-configuration card header, separating navigation from configuration writes.
- Model cards use one combined route badge instead of repeating OAuth and Sub2API-managed route text. The list consumes all remaining height above the activity log and shows roughly three models by default; long model names truncate before the action area instead of covering its controls.
- At the default window size, English top-bar controls automatically use compact labels so long text cannot overlap the five-stage quick-start slider.

### What changed in 1.6.4

- Expired OAuth recovery observations can no longer collapse into a one-second retry loop. Recovery probes run no more frequently than the 10-minute self-check, while the five-hour safety recovery limit remains in place.
- Stale recovery observations are removed when an account is no longer selected with an active same-model API fallback. Observation-file maintenance no longer masquerades as a live route change or causes duplicate routing and usage refreshes.
- Background recovery waits while usage, OAuth account loading, routing synchronization, configuration apply, OAuth sign-in, or service recovery is using the local admin API. This prevents transient OAuth account read failures caused by competing management requests; manual refresh still starts the unified self-check immediately.
- OAuth account recovery, fallback synchronization, and the Grok 4.6 manual suggestion were added. The later 1.6.5 fix prevents usage self-checks from auto-importing account model catalogs.
- OAuth recovery and fallback remain live Router backend changes. They do not rewrite the active Codex configuration, close Codex / ChatGPT, restart the client, or interrupt the current task.

### What changed in 1.6.3

- The proven v1.5.2 Codex login contract is restored. Applying Router keeps the existing ChatGPT sign-in, exposes the provider as `Codex-Router`, and keeps `requires_openai_auth = true`; the machine-local Router key still authenticates local forwarding while custom models load in the same signed-in UI.
- Every saved profile now has Apply and Delete actions. Delete requires confirmation and removes only that profile snapshot and its isolated API credentials; OAuth accounts and the current Codex configuration remain untouched. The active profile must be switched or reset before deletion.
- Restoring Codex defaults removes only Router-owned route fields and no longer deletes non-ChatGPT authentication files or unknown formats introduced by future Codex releases.
- OAuth authorization links for OpenAI, Claude, Gemini, Antigravity, and Grok now open through the Windows default HTTPS browser handler. Adding a second Grok or other provider account preserves the account chooser parameters and complete long URL instead of opening the Documents folder.

- When configuration isolation is enabled, manual saves, post-login OAuth synchronization, and Router-mode activation must target an existing bound profile. Background self-checks only read the Codex binding and report drift; they never overwrite `config.toml` or the model catalog, avoiding an unsolicited client reload.
- `chatgpt_oauth` mode keeps the stable `codex_router` provider ID and `Codex-Router` label. Compatibility apply and install scripts now write the same v1.5.2 account contract instead of overwriting it with a third-party API identity.
- Applying a new configuration first asks Codex Desktop to close gracefully. Only verified processes that remain after the timeout are terminated, children before parents; Electron children that already exited no longer make the restart report a false failure.
- Microsoft Store/MSIX installations are relaunched through the official AUMID with `shell:AppsFolder`, rather than by executing a protected EXE inside `WindowsApps`, so a cold start loads the new model catalog.
- `chatgpt_oauth` mode explicitly keeps the ChatGPT login method. If file-backed login state is unexpectedly missing, Codex-Router restores only a recent snapshot that the current Windows user can decrypt and validate, and never overwrites existing auth. Router requests continue to use the local Router Key instead of treating a ChatGPT token as that Key.

- Usage querying, local Sub2API reads, provider-response normalization, bounded concurrency, and last-good quota caching now run in Rust. Refreshing usage no longer starts a PowerShell process.
- Kimi, Grok, Z.ai/GLM, MiniMax, ZenMux, Volcengine Ark, MiMo, OpenRouter, and DeepSeek continue to refresh independently, so one provider failure does not fail the dashboard.
- Usage queries read Windows Credential Manager through references saved by the application. Keys, tokens, cookies, and account identities are not written to logs or test fixtures.
- Model catalog generation, route planning, and OAuth/API merge-split logic now run in Rust. Codex sees the model list the GUI produces directly instead of waiting for a PowerShell helper.
- With automatic continuity enabled, matching OAuth and API channels keep one public model ID and subscription quota is preferred. With it disabled, stable API and OAuth route IDs appear separately under the same display name so the user can choose the quota source.
- Duplicate OAuth account entries and unstable public model IDs caused by the old catalog builder are resolved.
- Codex TOML generation, validation, permission preservation, backup retention, and atomic writes now run in Rust. In split mode, a matching API default is written with its stable public route ID instead of accidentally selecting the OAuth route.
- OAuth account-level usage requests retry bounded transient failures before reporting `class=request_failure`.
- Successful Grok and Antigravity quota probes clear only recoverable historical probe errors. Antigravity quota reads use the current token provider, refresh and retry once after a 401, and render the live provider model catalog even when local model names have changed.
- Kimi `k3-256k` context-limit responses no longer disable a valid Coding Plan account as though its Key were invalid.
- Every manual or background usage refresh now checks selected disabled accounts for recovery. An account is re-enabled only after a fresh live quota response confirms usable capacity and no credential rejection; cached data never re-enables an invalid account.
- The self-check runs every 10 minutes by default, including in lightweight tray mode. It checks OAuth health, configuration binding, live token and coding-plan usage, and fallback eligibility. A live exhausted OAuth quota is made unschedulable immediately when a matching API fallback exists, and is re-enabled only after a fresh quota check confirms recovery; an unknown state is isolated for at most five hours before a recovery attempt. Binding drift is reported without automatic file replacement.
- OAuth-to-API fallback, recovery, and background discovery of new OAuth accounts are live backend route changes. Codex / ChatGPT is not closed or restarted, so the active task and conversation continue. Codex-Router shows a quota/fallback notification; only an explicit full Save & apply or profile switch may require a Codex restart. OAuth and API routes share one display name by default and do not add an `(OAuth)` suffix.
- OAuth account priorities, account recovery, OAuth login, configuration application, and the PostgreSQL, Redis, and Sub2API lifecycle now run natively in Rust. Redis readiness requires an authenticated `PONG`.
- The updater validates the official GitHub URL, SHA-256 digest, and release manifest while reporting live download progress. A detached Rust helper performs atomic replacement, rollback on failure, and automatic restart.
- A hidden local Router Key is recognized idempotently by its managed name and group, so repeated apply operations do not create duplicate Key records.
- The portable root includes `Start-Codex-Router.cmd`, a launcher shell that does not depend on PowerShell. EXE publisher metadata is consistently `Hernan_JIANG`; a trusted certificate issued to that publisher is still required for SmartScreen trust.
- The 1.6.11 portable package and installer payload contain no `.ps1`, `.psm1`, or `.psd1` files. PowerShell remains only in the GitHub source repository for Windows build, release, compatibility, and development tests.

## Download And First Run

Download the Windows x64 package from [GitHub Releases](https://github.com/HernanJiang/Codex-Router/releases/tag/v1.6.11):

`Codex-Router-Portable-1.6.11-windows-x64.zip`

An optional per-user installer is also provided as `Codex-Router-Installer-1.6.11-windows-x64.exe`. It uses the native Rust installer path to place the same verified runtime under `%LOCALAPPDATA%\Programs\Codex-Router\1.6.11` without administrator access.

For transient pre-output stream failures such as `Upstream request failed`, the router now allows up to five same-account retries by default with a longer 1.5-second interval. The request is never replayed after visible model output has started.

The package is portable and does not require Python, Node.js, Rust, PostgreSQL, Redis, or a separately installed VC++ runtime. Extract the complete directory before launching it. Do not move only the GUI executable out of the package.

The first launch opens on page one of the end-to-end guide. It walks through the project, login, model, network, and deployment steps, so a new installation has no separate setup manual or high learning cost.

<p align="center">
  <img src="assets/release/first-run-guide.png" alt="Codex-Router first-run setup guide" width="100%">
</p>

### Quick start

1. Extract the complete package and open `Codex-Router.exe`. If Windows shows SmartScreen for the unsigned EXE, the package also includes the `Start-Codex-Router.cmd` launcher shell.
2. Follow the first-run guide to add the first API channel or connect an OAuth subscription.
3. Add the models you want to the current routing profile.
4. Review the embedded usage and distribution terms, scroll to the end, and confirm them yourself.
5. Apply the configuration. Router initializes its local services and updates the Codex provider configuration.
6. Use the Codex model menu to switch models in the same context window.

The current release supports Windows 10/11 x64. ARM64 and macOS are not included in this Windows release.

## Security And Terms

- API keys, proxy passwords, and the local Router key are stored through Windows Credential Manager.
- OAuth tokens remain managed by Sub2API and are not copied into the Router configuration.
- Release packages exclude user configuration, logs, databases, OAuth state, backups, and developer paths.
- Runtime services bind to `127.0.0.1` by default. The management endpoint is not intended for remote exposure.
- The complete terms are available in [English](TERMS.en.md) and [中文](TERMS.zh-CN.md).
- Codex-Router original work is licensed under the GNU Affero General Public License v3.0 (AGPL-3.0). Sub2API and other third-party components remain subject to their upstream licenses and notices.

For the full directory layout and upgrade behavior, see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) and the release package README.

## Development

The desktop UI and usage-monitoring runtime are in `codex-router-gui-rust`. Routing and packaging helpers are in `scripts` and `source/backend`. The repository guide documents the supported validation commands. Do not package a live runtime directory containing user data or credentials.

Official repository: <https://github.com/HernanJiang/Codex-Router>
