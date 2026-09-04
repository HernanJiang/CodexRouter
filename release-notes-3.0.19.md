# CodexRouter v3.0.19

升级时不再双开 Host。Windows 凭据按 UserData 隔离，避免和 CraftStation 抢同一把钥匙。

## 修复

- 旧便携版 Host 仍占端口时，新版本接管同一 UserData 进程并在原端口重启，不再改去 28080 造成 `CR-CLI-0003` / `CR-CFG-0005`。
- Windows 凭据目标改为 `CodexRouter/{UserData指纹}/…`，读取仍兼容旧的 `CodexRouter/…`。
