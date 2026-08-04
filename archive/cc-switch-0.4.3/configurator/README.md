# Codex-Router 旧版静态界面

这是旧版静态界面的源码存档。它不再是受支持的配置入口，避免与正式程序产生不同的端口、认证和凭据行为。

## 使用方式

运行 `../scripts/Start-CodexRouter.ps1` 或直接打开根目录的 `Codex-Router.exe`。启动脚本会进入同一个 Rust 桌面程序，不需要 Python、Node.js 或 CDN。

## 功能

1. 添加模型渠道：模型名称、别名、Base URL、API Key、优先级、权重、其它参数。
2. 官方 OAuth / 第三方同名模型自动兜底开关。
3. 思考程度 / Fast 档位：手动填写或根据模型名自动匹配。
4. 一键生成：
   - `codex-router-config.json`：统一配置
   - `sub2api-channels.json`：Sub2API 渠道配置
   - `cc-switch-providers.json`：CC Switch Provider 隔离配置
5. 下载配置文件后，将其放入项目根目录，运行 `scripts/apply-codex-router.ps1` 完成部署。
