use anyhow::{bail, Context};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const LAYOUT_MARKER: &str = ".codex-router-user-data-v1";

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

fn candidate_score(path: &Path) -> u32 {
    let mut score = 0;
    if path.join("codex-router-config.json").is_file() {
        score += 1_000;
    }
    if path.join("backups").join("config-profiles").is_dir() {
        score += 500;
    }
    if path
        .join("data")
        .join("postgres")
        .join("PG_VERSION")
        .is_file()
    {
        score += 300;
    }
    if path.join("data").join("sub2api").is_dir() {
        score += 50;
    }
    if path.join("codex-router-ui-preferences.json").is_file() {
        score += 25;
    }
    score
}

fn candidate_modified(path: &Path) -> SystemTime {
    [
        path.join("codex-router-config.json"),
        path.join("codex-router-ui-preferences.json"),
        path.join("backups").join("config-profiles"),
        path.join("data").join("postgres").join("PG_VERSION"),
    ]
    .into_iter()
    .filter_map(|item| item.metadata().ok()?.modified().ok())
    .max()
    .unwrap_or(SystemTime::UNIX_EPOCH)
}

fn legacy_candidates(router_root: &Path, persistent_root: &Path) -> Vec<PathBuf> {
    let mut candidates = vec![router_root.to_path_buf()];
    if let Some(parent) = router_root.parent() {
        if let Ok(entries) = std::fs::read_dir(parent) {
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                if !path.is_dir() || path == router_root || path == persistent_root {
                    continue;
                }
                let portable_release = entry.file_name().to_str().is_some_and(|name| {
                    name.to_ascii_lowercase()
                        .starts_with("codex-router-portable-")
                });
                if portable_release {
                    candidates.push(path);
                }
            }
        }
    }
    candidates.retain(|path| candidate_score(path) > 0);
    candidates.sort_by(|left, right| {
        candidate_score(right)
            .cmp(&candidate_score(left))
            .then_with(|| candidate_modified(right).cmp(&candidate_modified(left)))
    });
    candidates
}

fn copy_file_if_present(source: &Path, destination: &Path) -> anyhow::Result<()> {
    if !source.is_file() || destination.exists() {
        return Ok(());
    }
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(source, destination)
        .with_context(|| format!("could not migrate {}", source.display()))?;
    Ok(())
}

fn copy_directory_if_present(source: &Path, destination: &Path) -> anyhow::Result<()> {
    if !source.is_dir() || destination.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = destination.join(entry.file_name());
        if file_type.is_symlink() {
            bail!(
                "refusing to migrate a linked user-data entry: {}",
                entry.path().display()
            );
        }
        if file_type.is_dir() {
            copy_directory_if_present(&entry.path(), &target)?;
        } else if file_type.is_file() {
            copy_file_if_present(&entry.path(), &target)?;
        }
    }
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

pub fn prepare(router_root: &Path) -> anyhow::Result<PathBuf> {
    let root = state_root(router_root);
    if root == router_root {
        return Ok(root);
    }
    std::fs::create_dir_all(&root)?;
    if root.join(LAYOUT_MARKER).is_file() {
        return Ok(root);
    }

    let source = legacy_candidates(router_root, &root).into_iter().next();
    if let Some(source) = source.as_deref() {
        copy_file_if_present(
            &source.join("codex-router-config.json"),
            &root.join("codex-router-config.json"),
        )?;
        copy_file_if_present(
            &source.join("codex-router-ui-preferences.json"),
            &root.join("codex-router-ui-preferences.json"),
        )?;
        copy_directory_if_present(&source.join("backups"), &root.join("backups"))?;
        // PostgreSQL contains OAuth accounts. Redis, PID files, locks, and UI
        // caches are deliberately rebuilt so an upgrade cannot inherit stale
        // process ownership or transient state.
        copy_directory_if_present(
            &source.join("data").join("postgres"),
            &root.join("data").join("postgres"),
        )?;
        copy_directory_if_present(
            &source.join("data").join("sub2api"),
            &root.join("data").join("sub2api"),
        )?;
    }
    write_layout_marker(&root, source.as_deref())?;
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
    fn legacy_candidate_prefers_configured_release_over_newer_empty_runtime() {
        let parent = temporary_test_dir("candidate");
        let current = parent.join("Codex-Router-Portable-1.2.1-test");
        let configured = parent.join("Codex-Router-Portable-1.1.8-test");
        let empty = parent.join("Codex-Router-Portable-1.2.0-test");
        let persistent = parent.join("stable");
        for path in [&current, &configured, &empty] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::write(configured.join("codex-router-config.json"), "{}").unwrap();
        std::fs::create_dir_all(empty.join("data").join("sub2api")).unwrap();
        std::fs::write(empty.join("codex-router-ui-preferences.json"), "{}").unwrap();
        let candidates = legacy_candidates(&current, &persistent);
        assert_eq!(candidates.first(), Some(&configured));
        std::fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn migration_copies_persistent_state_but_not_transient_process_files() {
        let parent = temporary_test_dir("migration");
        let release = parent.join("Codex-Router-Portable-1.2.0-test");
        let persistent = parent.join("stable");
        std::fs::create_dir_all(release.join("backups").join("config-profiles")).unwrap();
        std::fs::create_dir_all(release.join("data").join("postgres")).unwrap();
        std::fs::create_dir_all(release.join("data").join("pids")).unwrap();
        std::fs::write(release.join("codex-router-config.json"), "{}").unwrap();
        std::fs::write(
            release
                .join("backups")
                .join("config-profiles")
                .join("state.json"),
            "{}",
        )
        .unwrap();
        std::fs::write(
            release.join("data").join("postgres").join("PG_VERSION"),
            "18",
        )
        .unwrap();
        std::fs::write(release.join("data").join("pids").join("sub2api.pid"), "123").unwrap();

        std::fs::create_dir_all(&persistent).unwrap();
        let source = legacy_candidates(&release, &persistent).remove(0);
        copy_file_if_present(
            &source.join("codex-router-config.json"),
            &persistent.join("codex-router-config.json"),
        )
        .unwrap();
        copy_directory_if_present(&source.join("backups"), &persistent.join("backups")).unwrap();
        copy_directory_if_present(
            &source.join("data").join("postgres"),
            &persistent.join("data").join("postgres"),
        )
        .unwrap();

        assert!(persistent.join("codex-router-config.json").is_file());
        assert!(persistent
            .join("backups")
            .join("config-profiles")
            .join("state.json")
            .is_file());
        assert!(persistent
            .join("data")
            .join("postgres")
            .join("PG_VERSION")
            .is_file());
        assert!(!persistent.join("data").join("pids").exists());
        std::fs::remove_dir_all(parent).unwrap();
    }
}
