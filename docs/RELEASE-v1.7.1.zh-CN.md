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
