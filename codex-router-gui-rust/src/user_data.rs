use std::path::{Path, PathBuf};

const LAYOUT_MARKER: &str = ".codex-router-user-data-v1";
const PACKAGE_VERSION_MARKER: &str = ".codex-router-package-version";

fn stable_state_enabled(router_root: &Path) -> bool {
    if std::env::var_os("CODEX_ROUTER_PORTABLE_STATE").is_some_and(|value| value == "1") {
        return false;
    }
    std::env::var_os("CODEX_ROUTER_USER_DATA_ROOT").is_some()
        || router_root.join("release-manifest.json").is_file()
}

pub fn state_root(router_root: &Path) -> PathBuf {
    if !stable_state_enabled(router_root) {
        return router_root.to_path_buf();
    }
    if let Some(path) = std::env::var_os("CODEX_ROUTER_USER_DATA_ROOT") {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            return path;
        }
    }
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|path| path.join("Codex-Router").join("UserData"))
        .unwrap_or_else(|| router_root.to_path_buf())
}

pub fn config_path(router_root: &Path) -> PathBuf {
    state_root(router_root).join("codex-router-config.json")
}

pub fn preferences_path(router_root: &Path) -> PathBuf {
    state_root(router_root).join("codex-router-ui-preferences.json")
}

pub fn data_root(router_root: &Path) -> PathBuf {
    state_root(router_root).join("data")
}

pub fn backups_root(router_root: &Path) -> PathBuf {
    state_root(router_root).join("backups")
}

fn package_version(router_root: &Path) -> Option<String> {
    let manifest = router_root.join("release-manifest.json");
    if !manifest.is_file() {
        return Some(env!("CARGO_PKG_VERSION").to_owned());
    }
    let text = std::fs::read_to_string(manifest).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value
        .get("version")
        .and_then(|item| item.as_str())
        .map(str::to_owned)
        .or_else(|| Some(env!("CARGO_PKG_VERSION").to_owned()))
}

fn stored_package_version(root: &Path) -> Option<String> {
    std::fs::read_to_string(root.join(PACKAGE_VERSION_MARKER))
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn write_package_version(root: &Path, version: &str) -> anyhow::Result<()> {
    crate::config::atomic_write(
        &root.join(PACKAGE_VERSION_MARKER),
        format!("{version}\n").as_bytes(),
    )
}

/// Record the package version without modifying stable user data. Portable
/// releases share this directory, so deleting it during an upgrade destroys
/// the configuration and OAuth database the next package must reuse.
pub fn migrate_package_version(router_root: &Path) -> anyhow::Result<()> {
    let Some(version) = package_version(router_root) else {
        return Ok(());
    };
    let root = state_root(router_root);
    if root == router_root {
        let _ = write_package_version(&root, &version);
        return Ok(());
    }
    std::fs::create_dir_all(&root)?;
    if stored_package_version(&root).as_deref() == Some(version.as_str()) {
        return Ok(());
    }
    write_package_version(&root, &version)?;
    Ok(())
}

fn write_layout_marker(root: &Path, source: Option<&Path>) -> anyhow::Result<()> {
    let marker = root.join(LAYOUT_MARKER);
    let temporary = root.join(format!("{LAYOUT_MARKER}.{}.tmp", std::process::id()));
    let source_kind = if source.is_some() {
        "legacy_portable"
    } else {
        "new_install"
    };
    std::fs::write(&temporary, format!("layout=1\nsource={source_kind}\n"))?;
    if marker.exists() {
        std::fs::remove_file(&marker)?;
    }
    std::fs::rename(&temporary, &marker)?;
    Ok(())
}

/// True when the saved config represents a finished first-run setup, not a
/// leftover empty/partial JSON that should still show the welcome wizard.
pub fn config_looks_configured(config: &crate::config::RouterConfig) -> bool {
    !config.accepted_terms_version.trim().is_empty()
        && config.accept_compliance
        && !config.models.is_empty()
}

pub fn prepare(router_root: &Path) -> anyhow::Result<PathBuf> {
    migrate_package_version(router_root)?;
    let root = state_root(router_root);
    if root == router_root {
        return Ok(root);
    }
    std::fs::create_dir_all(&root)?;
    if root.join(LAYOUT_MARKER).is_file() {
        return Ok(root);
    }

    write_layout_marker(&root, None)?;
    if let Some(version) = package_version(router_root) {
        let _ = write_package_version(&root, &version);
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_test_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "codex-router-user-data-{label}-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ))
    }

    #[test]
    fn source_and_test_roots_remain_self_contained() {
        let root = temporary_test_dir("self-contained");
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(state_root(&root), root);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn empty_config_is_not_treated_as_configured() {
        let config = crate::config::RouterConfig::default();
        assert!(!config_looks_configured(&config));
    }

    #[test]
    fn package_version_update_preserves_all_user_data() {
        let root = temporary_test_dir("preserve-upgrade");
        std::fs::create_dir_all(root.join("data").join("postgres")).unwrap();
        std::fs::create_dir_all(root.join("backups")).unwrap();
        std::fs::write(root.join("codex-router-config.json"), b"config").unwrap();
        std::fs::write(root.join("codex-router-ui-preferences.json"), b"prefs").unwrap();
        std::fs::write(root.join("data").join("postgres").join("PG_VERSION"), b"17").unwrap();
        std::fs::write(root.join("backups").join("point.json"), b"backup").unwrap();
        write_package_version(&root, "1.2.15").unwrap();

        write_package_version(&root, "1.2.17").unwrap();

        assert_eq!(stored_package_version(&root).as_deref(), Some("1.2.17"));
        assert_eq!(
            std::fs::read(root.join("codex-router-config.json")).unwrap(),
            b"config"
        );
        assert_eq!(
            std::fs::read(root.join("codex-router-ui-preferences.json")).unwrap(),
            b"prefs"
        );
        assert!(root
            .join("data")
            .join("postgres")
            .join("PG_VERSION")
            .is_file());
        assert!(root.join("backups").join("point.json").is_file());
        std::fs::remove_dir_all(root).unwrap();
    }
}
