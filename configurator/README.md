# Codex Router Configurator

纯前端配置器，使用 Vue 3 + Tailwind CSS CDN，无需构建步骤。

## 使用方式

```bash
# 方式 1：PowerShell
& ../scripts/Start-Configurator.ps1

# 方式 2：Python
cd configurator
python -m http.server 8080
```

然后在浏览器打开 http://127.0.0.1:8080。

## 功能

1. 添加模型渠道：模型名称、别名、Base URL、API Key、优先级、权重、其它参数。
2. 官方 OAuth / 第三方同名模型自动兜底开关。
3. 思考程度 / Fast 档位：手动填写或根据模型名自动匹配。
4. 一键生成：
   - `codex-router-config.json`：统一配置
   - `sub2api-channels.json`：Sub2API 渠道配置
   - `cc-switch-providers.json`：CC Switch Provider 隔离配置
5. 下载配置文件后，将其放入项目根目录，运行 `scripts/apply-codex-router.ps1` 完成部署。
