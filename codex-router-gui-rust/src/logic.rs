use crate::config::{ModelConfig, RouterConfig};
use anyhow::{bail, Context};
use serde_json::json;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub fn detect_reasoning(model_name: &str) -> (Vec<String>, String, bool) {
    let name = model_name.to_lowercase();
    let values = |items: &[&str]| items.iter().map(|s| (*s).to_string()).collect();
    if name.contains("gpt-5.6-sol") {
        return (
            values(&["low", "medium", "high", "xhigh", "max", "ultra"]),
            "low".into(),
            true,
        );
    }
    if name.contains("gpt-5.6-terra") {
        return (
            values(&["low", "medium", "high", "xhigh", "max", "ultra"]),
            "medium".into(),
            true,
        );
    }
    if name.contains("gpt-5.6-luna") {
        return (
            values(&["low", "medium", "high", "xhigh", "max"]),
            "medium".into(),
            true,
        );
    }
    if name.contains("grok") {
        return (
            values(&["minimal", "low", "medium", "high", "xhigh"]),
            "medium".into(),
            false,
        );
    }
    if name.contains("deepseek") {
        return (
            values(&["minimal", "low", "medium", "high", "xhigh"]),
            "low".into(),
            false,
        );
    }
    (vec![], String::new(), false)
}

pub fn detect_multimodal(model_name: &str) -> bool {
    let name = model_name.to_lowercase();
    let markers = [
        "gpt-4o",
        "gpt-4.5",
        "gpt-5",
        "claude-3",
        "claude-opus",
        "claude-sonnet",
        "gemini",
        "kimi",
        "k3",
        "grok-3",
        "grok-4",
        "qwen-vl",
        "qwen2-vl",
        "qwen2.5-vl",
        "llava",
        "yi-vision",
        "internvl",
        "minicpm",
        "glm-4v",
        "glm4v",
    ];
    markers.iter().any(|m| name.contains(m))
}

pub fn resolve_multimodal(model: &ModelConfig) -> bool {
    match model.multimodal.as_str() {
        "true" => true,
        "false" => false,
        _ => detect_multimodal(&model.model),
    }
}

pub fn detect_cc_switch_db() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".cc-switch").join("cc-switch.db"));
    }
    if let Some(config_dir) = dirs::config_dir() {
        candidates.extend([
            config_dir.join("com.ccswitch.desktop").join("cc-switch.db"),
            config_dir.join("CC Switch").join("cc-switch.db"),
            config_dir.join("cc-switch").join("cc-switch.db"),
        ]);
    }
    if let Some(data_dir) = dirs::data_local_dir() {
        candidates.extend([
            data_dir.join("com.ccswitch.desktop").join("cc-switch.db"),
            data_dir.join("CC Switch").join("cc-switch.db"),
            data_dir.join("cc-switch").join("cc-switch.db"),
        ]);
    }
    if let Some(custom_home) = std::env::var_os("CC_SWITCH_HOME") {
        candidates.insert(0, PathBuf::from(custom_home).join("cc-switch.db"));
    }
    candidates.into_iter().find(|path| path.is_file())
}

pub fn slugify(value: &str) -> String {
    let slug = value
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .to_lowercase();
    if slug.is_empty() {
        "model".to_string()
    } else {
        slug
    }
}

pub fn build_model_catalog(cfg: &RouterConfig) -> Vec<serde_json::Value> {
    cfg.models
        .iter()
        .enumerate()
        .map(|(index, model)| {
            let (levels, default_level, fast) = if cfg.reasoning.mode == "manual" {
                (
                    cfg.reasoning.levels.clone(),
                    cfg.reasoning.default_level.clone(),
                    cfg.reasoning.supports_fast,
                )
            } else {
                detect_reasoning(&model.model)
            };
            let reasoning_levels: Vec<_> = levels
                .iter()
                .map(|effort| json!({"effort": effort, "description": format!("{} reasoning level", effort)}))
                .collect();
            json!({
                "slug": model.model,
                "display_name": if model.alias.is_empty() { &model.model } else { &model.alias },
                "description": format!("Codex-Router model #{}", index + 1),
                "default_reasoning_level": default_level,
                "supported_reasoning_levels": reasoning_levels,
                "supports_vision": resolve_multimodal(model),
                "shell_type": "shell_command",
                "visibility": "list",
                "supported_in_api": true,
                "priority": model.priority,
                "additional_speed_tiers": if fast { vec!["fast"] } else { vec![] },
            })
        })
        .collect()
}

/// This manifest intentionally contains credential references, never API keys.
pub fn build_channel_manifest(cfg: &RouterConfig) -> Vec<serde_json::Value> {
    cfg.models
        .iter()
        .map(|model| {
            json!({
                "name": if model.alias.is_empty() { &model.model } else { &model.alias },
                "type": "openai",
                "base_url": model.base_url,
                "credential": model.credential_name,
                "models": [model.model.clone()],
                "priority": model.priority,
                "weight": model.weight,
                "supports_vision": resolve_multimodal(model),
                "extra": serde_json::from_str::<serde_json::Value>(&model.extra).unwrap_or_else(|_| json!({})),
            })
        })
        .collect()
}

pub fn write_all_files(cfg: &RouterConfig, router_root: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(router_root.join("config"))?;
    cfg.save(&router_root.join("codex-router-config.json"))?;
    std::fs::write(
        router_root.join("config").join("model-catalog.json"),
        serde_json::to_string_pretty(&build_model_catalog(cfg))?,
    )?;
    std::fs::write(
        router_root.join("config").join("sub2api-channels.json"),
        serde_json::to_string_pretty(&build_channel_manifest(cfg))?,
    )?;
    Ok(())
}

fn ps_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn run_powershell_stdin(script: &str) -> anyhow::Result<String> {
    let mut child = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x08000000)
        .spawn()
        .context("无法启动 Windows PowerShell")?;
    child
        .stdin
        .as_mut()
        .context("无法打开 PowerShell 标准输入")?
        .write_all(script.as_bytes())?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!(
            "PowerShell 执行失败: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn store_credentials(cfg: &mut RouterConfig, router_root: &Path) -> anyhow::Result<()> {
    let module = router_root.join("scripts").join("CredentialStore.psm1");
    let mut script = format!(
        "$ErrorActionPreference='Stop'\nImport-Module {} -Force\n",
        ps_literal(&module.to_string_lossy())
    );
    for (index, model) in cfg.models.iter_mut().enumerate() {
        if model.credential_name.trim().is_empty() {
            model.credential_name = format!("ModelApiKey-{}-{}", index + 1, slugify(&model.model));
        }
        if !model.api_key.trim().is_empty() {
            script.push_str(&format!(
                "Set-RouterCredential -Name {} -Secret {}\n",
                ps_literal(&model.credential_name),
                ps_literal(model.api_key.trim())
            ));
        }
    }
    if cfg.proxy.password_credential.trim().is_empty() {
        cfg.proxy.password_credential = "ProxyPassword".to_string();
    }
    if !cfg.proxy.password.is_empty() {
        script.push_str(&format!(
            "Set-RouterCredential -Name {} -Secret {}\n",
            ps_literal(&cfg.proxy.password_credential),
            ps_literal(&cfg.proxy.password)
        ));
    }
    script.push_str("'credentials-saved'\n");
    let _ = run_powershell_stdin(&script)?;
    for model in &mut cfg.models {
        model.api_key.clear();
    }
    cfg.proxy.password.clear();
    cfg.local_api_key.clear();
    Ok(())
}

pub fn read_credential(router_root: &Path, name: &str) -> anyhow::Result<String> {
    let module = router_root.join("scripts").join("CredentialStore.psm1");
    run_powershell_stdin(&format!(
        "$ErrorActionPreference='Stop'\nImport-Module {} -Force\nGet-RouterCredential -Name {}\n",
        ps_literal(&module.to_string_lossy()),
        ps_literal(name)
    ))
}

pub fn run_apply_script<F>(router_root: &Path, mut on_line: F) -> anyhow::Result<()>
where
    F: FnMut(String),
{
    let script = router_root.join("scripts").join("Apply-Configurator.ps1");
    let mut command = Command::new("powershell.exe");
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script)
        .current_dir(router_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x08000000);
    let mut child = command
        .spawn()
        .with_context(|| format!("无法运行 {}", script.display()))?;

    let stdout = child.stdout.take().context("无法读取部署脚本输出")?;
    let stderr = child.stderr.take().context("无法读取部署脚本错误输出")?;
    let (line_tx, line_rx) = mpsc::channel::<(bool, String)>();
    let stdout_tx = line_tx.clone();
    let stdout_reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if stdout_tx.send((false, line)).is_err() {
                break;
            }
        }
    });
    let stderr_reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if line_tx.send((true, line)).is_err() {
                break;
            }
        }
    });

    let started = Instant::now();
    let timeout = Duration::from_secs(180);
    let mut stderr_tail = VecDeque::with_capacity(12);
    let status = loop {
        while let Ok((is_error, line)) = line_rx.try_recv() {
            if !line.trim().is_empty() {
                if is_error {
                    if stderr_tail.len() == 12 {
                        stderr_tail.pop_front();
                    }
                    stderr_tail.push_back(line.clone());
                }
                on_line(line);
            }
        }
        if let Some(status) = child.try_wait().context("无法读取部署脚本状态")? {
            break status;
        }
        if started.elapsed() >= timeout {
            let process_id = child.id().to_string();
            let _ = Command::new("taskkill.exe")
                .args(["/PID", &process_id, "/T", "/F"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(0x08000000)
                .status();
            let _ = child.wait();
            bail!("部署超过 180 秒，已自动停止。请查看界面中的最后一个部署阶段。");
        }
        std::thread::sleep(Duration::from_millis(60));
    };

    let _ = stdout_reader.join();
    let _ = stderr_reader.join();
    while let Ok((is_error, line)) = line_rx.try_recv() {
        if !line.trim().is_empty() {
            if is_error {
                if stderr_tail.len() == 12 {
                    stderr_tail.pop_front();
                }
                stderr_tail.push_back(line.clone());
            }
            on_line(line);
        }
    }

    if !status.success() {
        let details = stderr_tail.into_iter().collect::<Vec<_>>().join("\n");
        bail!(
            "一键配置失败（退出代码 {}）: {}",
            status.code().unwrap_or(-1),
            if details.trim().is_empty() {
                "部署脚本未返回详细错误"
            } else {
                details.trim()
            }
        );
    }
    Ok(())
}

pub fn sync_cc_switch(cfg: &RouterConfig, local_api_key: &str) -> anyhow::Result<()> {
    use rusqlite::{params, Connection};
    let db_path = if cfg.deploy.cc_switch_db.trim().is_empty() {
        detect_cc_switch_db().context("未在常用位置检测到 CC Switch 数据库")?
    } else {
        Path::new(&cfg.deploy.cc_switch_db).to_path_buf()
    };
    if !db_path.exists() {
        bail!("未找到 CC Switch 数据库: {}", db_path.display());
    }
    let backup_dir = db_path.parent().unwrap_or(Path::new(".")).join("backups");
    std::fs::create_dir_all(&backup_dir)?;
    let backup = backup_dir.join(format!(
        "cc-switch-before-codex-router-{}.db",
        chrono::Local::now().format("%Y%m%d-%H%M%S-%3f")
    ));

    let codex_home = if cfg.deploy.codex_home.trim().is_empty() {
        dirs::home_dir()
            .unwrap_or_else(|| Path::new(".").into())
            .join(".codex")
    } else {
        Path::new(&cfg.deploy.codex_home).to_path_buf()
    };
    let config_text = std::fs::read_to_string(codex_home.join("config.toml"))?;
    let settings = json!({
        "auth": {"auth_mode": "apikey", "OPENAI_API_KEY": local_api_key},
        "config": config_text,
    });
    let connection = Connection::open(&db_path)?;
    let backup_sql = format!(
        "VACUUM INTO '{}'",
        backup.to_string_lossy().replace('\'', "''")
    );
    connection
        .execute_batch(&backup_sql)
        .with_context(|| format!("无法备份 CC Switch 数据库到 {}", backup.display()))?;
    connection.execute(
        "INSERT INTO providers (id, app_type, name, settings_config) \
         VALUES (?1, 'codex', 'Codex-Router（隔离配置）', ?2) \
         ON CONFLICT(id, app_type) DO UPDATE SET name=excluded.name, settings_config=excluded.settings_config",
        params!["codex-router-isolated", serde_json::to_string(&settings)?],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multimodal_can_be_auto_detected_or_overridden() {
        let mut model = ModelConfig {
            model: "kimi-k3".into(),
            ..Default::default()
        };
        assert!(resolve_multimodal(&model));
        model.multimodal = "false".into();
        assert!(!resolve_multimodal(&model));
        model.model = "custom-vision-model".into();
        model.multimodal = "true".into();
        assert!(resolve_multimodal(&model));
    }

    #[test]
    fn channel_manifest_contains_reference_not_secret() {
        let mut config = RouterConfig::default();
        config.models.push(ModelConfig {
            model: "test".into(),
            base_url: "https://example.invalid/v1".into(),
            api_key: "sk-do-not-write".into(),
            credential_name: "ModelApiKey-test".into(),
            ..Default::default()
        });
        let json = serde_json::to_string(&build_channel_manifest(&config)).unwrap();
        assert!(json.contains("ModelApiKey-test"));
        assert!(!json.contains("sk-do-not-write"));
    }
}
