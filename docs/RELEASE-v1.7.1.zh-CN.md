# CodexRouter v1.7.1

发布日期：2026-08-15

## 本版本重点

- Windows 安装器改为安装向导。用户先选择安装位置，默认创建桌面快捷方式，再点击确认安装；不会一点就直接装完。
- 公开项目名称和官方仓库地址统一为 `CodexRouter`。内部可执行文件名和用户数据目录仍兼容现有 `Codex-Router` 路径。
- 额外提供 macOS / Linux 理论构建包。这些包尚未在真实 macOS 或 Linux 机器上测试，欢迎更多用户参与共同构建。

## 安装向导

Windows 安装包 `Codex-Router-Installer-1.7.1-windows-x64.exe` 会打开 CodexRouter 安装向导：

1. 选择安装位置，默认路径为 `%LOCALAPPDATA%\Programs\CodexRouter\1.7.1`
2. 默认勾选“创建桌面快捷方式（同时加入开始菜单）”
3. 点击“确认安装”后才开始复制文件
4. 安装不会覆盖个人配置和运行数据

便携版仍可解压后直接运行。

## 下载

GitHub Release 提供以下产物：

- `Codex-Router-Installer-1.7.1-windows-x64.exe`：Windows 安装版
- `Codex-Router-Portable-1.7.1-windows-x64.zip`：Windows 便携版
- `CodexRouter-1.7.1-linux-x64-theoretical.tar.gz`：Linux 理论构建
- `CodexRouter-1.7.1-macos-arm64-theoretical.tar.gz`：macOS Apple Silicon 理论构建
- `CodexRouter-1.7.1-macos-x64-theoretical.tar.gz`：macOS Intel 理论构建
- `SHA256SUMS.txt`：产物校验值

## 理论构建说明

macOS 和 Linux 为理论构建版本，未经过实际测试。当前受支持的运行时仍是 Windows 10/11 x64。欢迎更多用户参与共同构建。

## 已知限制

Windows 产物当前未进行代码签名，首次运行时可能出现 SmartScreen 提示。请从项目 GitHub Release 下载，并使用 `SHA256SUMS.txt` 校验文件完整性。

---

# CodexRouter v1.7.1

Release date: 2026-08-15

## Highlights

- The Windows installer is now a setup wizard. Choose the install location, keep the desktop shortcut by default, then confirm before installation starts. It no longer finishes immediately after a single click.
- The public project name and official repository are unified as `CodexRouter`. Internal executable names and user data directories remain compatible with existing `Codex-Router` paths.
- This release also publishes theoretical macOS / Linux packages. They have not been tested on real macOS or Linux machines. Contributions that help build and verify them are welcome.

## Setup wizard

The Windows installer `Codex-Router-Installer-1.7.1-windows-x64.exe` opens the CodexRouter setup wizard:

1. Choose the install location. The default path is `%LOCALAPPDATA%\Programs\CodexRouter\1.7.1`
2. "Create a desktop shortcut (also add to the Start menu)" is checked by default
3. Files are copied only after you click "Confirm install"
4. Installation does not overwrite personal configuration or runtime data

The portable package can still be extracted and run directly.

## Downloads

GitHub Release provides these artifacts:

- `Codex-Router-Installer-1.7.1-windows-x64.exe`: Windows installer
- `Codex-Router-Portable-1.7.1-windows-x64.zip`: Windows portable package
- `CodexRouter-1.7.1-linux-x64-theoretical.tar.gz`: Linux theoretical build
- `CodexRouter-1.7.1-macos-arm64-theoretical.tar.gz`: macOS Apple Silicon theoretical build
- `CodexRouter-1.7.1-macos-x64-theoretical.tar.gz`: macOS Intel theoretical build
- `SHA256SUMS.txt`: artifact checksums

## Theoretical builds

macOS and Linux packages are theoretical builds and have not been tested on real machines. The currently supported runtime remains Windows 10/11 x64. Contributions that help build and verify them are welcome.

## Known limitations

Windows artifacts are currently unsigned. SmartScreen may appear on first run. Please download from the project GitHub Release and verify integrity with `SHA256SUMS.txt`.
