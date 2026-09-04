# CodexRouter

本文件是所有 Agent 共用的项目约定。面向用户的说明默认中文。

## 目录结构（必须遵守）

```
D:\Work\CodexRouter\          唯一源码根 = git main。日常开发直接改这里。
  AGENTS.md
  README.md / CHANGELOG.md / TERMS.*
  app\                        打包用的 CLIProxy 等二进制
  assets\ scripts\ licenses\ docs\
  codex-router-gui-rust\      Rust 源码（单元测试也在这里）
  Release\                    便携包。每一版打到 Release\<version>\
  Test\                       验收报告输出目录（Agent_Test_<版本>_<模型>.md）
```

| 位置 | 角色 |
| --- | --- |
| 仓库根 `D:\Work\CodexRouter\` | **main**。唯一源码工作区。修 bug、做功能、跑 `cargo test` / `cargo build`、打包都在这里。 |
| `Release\<version>\` | 该版本的便携包。日常测试包和可上传包都打到这里；可上传包不得含用户信息。 |
| `Test\` | 只放新的验收报告。不要当测试套件。 |

禁止：

- **不要再创建或使用 `dev\`。** 以前的独立开发工作区已经取消。不要把源码、测试包或实验放进 `dev\`。
- 不要再使用 `Source\` 作为源码根。
- 不要只改源码就结束任务：用户没有新 exe 无法测试。
- 不要把 `.playwright-mcp\`、`ai_workspace\`、`target\`、`postgres\`、`redis\`、本机 UserData 当仓库内容保留。
- 不要同时跑两套不同版本的 Host。新包起来后只保留这一套进程。

当前入口：

- 稳定 / 测试同一套：`D:\Work\CodexRouter\Release\<version>\Codex-Router-Portable-<version>-windows-x64\Codex-Router.exe`
- 当前版本：**3.2.9**

## 每一版都必须打包 exe（默认）

在仓库根完成一版可测改动后，**必须立刻打包 Windows 便携 exe，停掉旧实例，打开新包给用户测试**。不要只改源码、只跑 `cargo test` 就收工。

1. 升高 `codex-router-gui-rust\Cargo.toml` 版本，更新 CHANGELOG / TERMS。
2. 从仓库根打包：

```
powershell -NoProfile -ExecutionPolicy Bypass -File D:\Work\CodexRouter\scripts\Build-PortableRelease.ps1 -OutputRoot D:\Work\CodexRouter\Release\<version>
```

3. 产物必须是：
   `D:\Work\CodexRouter\Release\<version>\Codex-Router-Portable-<version>-windows-x64\Codex-Router.exe`
4. 停掉本机所有 `Codex-Router.exe` / `codex-router-host.exe` / `cli-proxy-api.exe`，**立刻用新包的 `Codex-Router.exe` 打开 GUI 窗口**（工作目录为便携包根目录），把窗口恢复并置顶到用户桌面。只起 Host 进程、不弹出窗口，不算完成。确认 Host 健康检查通过且只有这一套进程。
5. 可上传包必须**不含用户个人信息**：全树敏感信息扫描必须通过；不打进 UserData、OAuth、API Key、本机绝对路径。
6. 用户说「先别切 / 不要重启」时：仍然打包到 `Release\`，但不替换正在跑的实例，并明确告诉用户测试包路径。
7. 新版已经打开并确认可用后，删除已被替代的旧 `Release\<旧版本>\`，避免再双开。仓库根只保留当前这一版便携包。

## 构建与安全

- 产品是 Rust `eframe` GUI + Windows 脚本。真正的自动化测试在 `codex-router-gui-rust` 的 `#[cfg(test)]`，不在 `Test\` 旧报告里。
- 在 crate 目录执行：`cargo fmt --all`、`cargo check --locked`、`cargo test --locked`、`cargo clippy --locked --all-targets -- -D warnings`。
- 用量逻辑在 `codex-router-gui-rust\src\logic\usage.rs`。发布包不得包含 `Get-UsageMonitor.ps1`。
- 禁止打印、写入测试夹具或打进包：API Key、OAuth token、cookie、AK/SK、账号邮箱、未脱敏的鉴权响应。
- 未经用户明确授权，不得 commit / push / 发布 GitHub Release。

## 关闭与端口

关闭应用必须停掉同名 `codex-router-host.exe` / `cli-proxy-api.exe` 占用的 18080、28080 段端口，不能把旧便携版 Host 留在后台。
