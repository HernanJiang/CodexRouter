# CodexRouter

本文件是所有 Agent 共用的项目约定。面向用户的说明默认中文。

## 目录结构（必须遵守）

```
D:\Work\CodexRouter\          稳定 main：源码直接展开在仓库根，不要再放进 Source\
  AGENTS.md
  README.md / CHANGELOG.md / TERMS.*
  app\                        打包用的 CLIProxy 等二进制
  assets\ scripts\ licenses\ docs\
  codex-router-gui-rust\      Rust 源码（单元测试也在这里）
  Release\                    稳定 main 的最终便携包（可上传）
  dev\                        开发工作区（未并入 main 的改动只放这里，已 gitignore）
    Release\                  开发测试便携包（每一版都要打，给用户实测）
  Test\                       验收报告输出目录（Agent_Test_<版本>_<模型>.md）
```

| 位置 | 角色 |
| --- | --- |
| 仓库根 `D:\Work\CodexRouter\` | 稳定 **main**。已确认可用的源码。不要把未确认的实验直接改在这里。 |
| `dev\` | **开发版本**。日常修 bug、做功能、跑 `cargo test` / `cargo build` 都在这里。 |
| `dev\Release\<version>\` | **测试便携包**。每一版改完都必须打 exe，给用户打开实测。 |
| `Release\<version>\` | 稳定 main 打出来的、**可上传**的干净便携包（不含用户信息）。 |
| `Test\` | 只放新的验收报告。不要当测试套件。 |

禁止：

- 不要再使用 `Source\` 作为源码根。
- 不要把开发中的包和稳定包混在同一套正在运行的 Host 里。
- 不要只改源码就结束任务：用户没有新 exe 无法测试。
- 不要把 `.playwright-mcp\`、`ai_workspace\`、`target\`、`postgres\`、`redis\`、本机 UserData 当仓库内容保留。

当前入口：

- 最近一次稳定 main：`D:\Work\CodexRouter\Release\3.1.0\Codex-Router-Portable-3.1.0-windows-x64\Codex-Router.exe`
- 当前测试包：`D:\Work\CodexRouter\dev\Release\<version>\Codex-Router-Portable-<version>-windows-x64\Codex-Router.exe`

## 每一版都必须打包 exe（默认）

在 `dev` 完成一版可测改动后，**必须立刻打包 Windows 便携 exe，并把新包的完整路径交给用户自行启动和切换**。不得因为打包或验收而擅自停止、重启、接管正在运行的旧实例；不要只改源码、只跑 `cargo test` 就收工。

1. 升高 `dev\codex-router-gui-rust\Cargo.toml` 版本。
2. 从 `dev` 打包（测试包，不是上传包）：

```
powershell -NoProfile -ExecutionPolicy Bypass -File D:\Work\CodexRouter\dev\scripts\Build-PortableRelease.ps1 -OutputRoot D:\Work\CodexRouter\dev\Release\<version>
```

3. 产物必须是：
   `D:\Work\CodexRouter\dev\Release\<version>\Codex-Router-Portable-<version>-windows-x64\Codex-Router.exe`
4. **不得停止或重启**本机任何 `Codex-Router.exe` / `codex-router-host.exe` / `cli-proxy-api.exe`，也不得替换正在运行的实例。打包后只需提供新包的完整路径和健康检查命令，由用户自行用新包的 `Codex-Router.exe` 启动 GUI 并切换。只有用户明确授权时，才可以执行停止、重启或接管；只起 Host 进程、不弹出窗口，不算用户已完成切换。
5. 测试包可以含本机构建痕迹，但仍然禁止打进 UserData、OAuth、API Key。它**不是**可公开发布包。
6. 用户说「先别切 / 不要重启」时：仍然打包到 `dev\Release\`，但不替换正在跑的实例，并明确告诉用户测试包路径。

## 并入 main（可上传的干净包）

只有用户明确说 **「并入 main」** 之后才做下面整套交付：

- 把 `dev` 里已稳定的改动合并进仓库根（不要带上本机 UserData、账号、密钥、本地路径、`target\`、日志、`dev` 自己的副本）。
- 升高根目录 `codex-router-gui-rust\Cargo.toml` 版本，更新 CHANGELOG / TERMS / README.zh-CN。
- 用**根目录** `scripts\Build-PortableRelease.ps1` 打包到
  `D:\Work\CodexRouter\Release\<version>\Codex-Router-Portable-<version>-windows-x64`。
- 产物必须是**不含用户个人信息**、可公开发布的便携包：全树敏感信息扫描必须通过；不打进 UserData、OAuth、API Key、本机绝对路径。
- 不得停掉或替换本机正在运行的 Router 进程；构建完成后提供新的 `Release` 包完整路径和健康检查命令，由用户自行打开并切换。只有用户明确授权时，才可以执行停止、重启或接管。
- 删除已被替代的旧 `Release\<旧版本>\`，避免再双开。不要删 `dev\Release\` 里用户还在测的包，除非用户说可以删。

可上传包命令（从仓库根，即稳定 main）：

```
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\Build-PortableRelease.ps1 -OutputRoot .\Release\<version>
```

不要把 `dev` 的测试包直接复制进仓库根 `Release\` 冒充可上传产物。

## 构建与安全

- 产品是 Rust `eframe` GUI + Windows 脚本。真正的自动化测试在 `codex-router-gui-rust` 的 `#[cfg(test)]`，不在 `Test\` 旧报告里。
- 在对应 crate 目录执行：`cargo fmt --all`、`cargo check --locked`、`cargo test --locked`、`cargo clippy --locked --all-targets -- -D warnings`。
- 用量逻辑在 `codex-router-gui-rust\src\logic\usage.rs`。发布包不得包含 `Get-UsageMonitor.ps1`。
- 禁止打印、写入测试夹具或打进包：API Key、OAuth token、cookie、AK/SK、账号邮箱、未脱敏的鉴权响应。
- 未经用户明确授权，不得 commit / push / 发布 GitHub Release。

## 关闭与端口

只有用户明确要求关闭、重启或切换实例时，才停止同名 `codex-router-host.exe` / `cli-proxy-api.exe` 及其占用的 18080、28080 段端口；日常构建、验收和发布不得触碰正在运行的旧便携版 Host，避免中断用户会话。
