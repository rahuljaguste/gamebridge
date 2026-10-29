// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Rahul Pramod Jaguste and contributors
//! Persistent host configuration.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use chiaki_core::target::Target;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredHost {
    pub id: String,
    pub name: String,
    pub host: String,
    /// "ps4_8", "ps4_9", "ps4_10", "ps5_1"
    pub target: String,
    /// Wire regist key, lowercase hex (32 chars).
    pub regist_key: String,
    /// RP-Key / "morning", lowercase hex (32 chars).
    pub morning: String,
    pub rp_key_type: u32,
    /// Server MAC, lowercase hex (12 chars).
    pub server_mac: String,
    pub nickname: String,
}

impl StoredHost {
    pub fn target_enum(&self) -> Result<Target> {
        parse_target(&self.target)
    }

    pub fn regist_key_bytes(&self) -> Result<[u8; 16]> {
        hex16(&self.regist_key, "regist key")
    }

    pub fn morning_bytes(&self) -> Result<[u8; 16]> {
        hex16(&self.morning, "morning")
    }
}

fn hex16(value: &str, field: &str) -> Result<[u8; 16]> {
    let bytes = hex::decode(value).with_context(|| format!("invalid {field}"))?;
    if bytes.len() != 16 {
        anyhow::bail!("{field} must be 16 bytes");
    }
    let mut out = [0u8; 16];
    out.copy_from_slice(&bytes);
    Ok(out)
}

pub fn target_string(target: Target) -> &'static str {
    match target {
        Target::Ps4_8 => "ps4_8",
        Target::Ps4_9 => "ps4_9",
        Target::Ps4_10 => "ps4_10",
        Target::Ps5_1 => "ps5_1",
        Target::Ps4Unknown => "ps4_10",
        Target::Ps5Unknown => "ps5_1",
    }
}

pub fn parse_target(s: &str) -> Result<Target> {
    match s {
        "ps4_8" => Ok(Target::Ps4_8),
        "ps4_9" => Ok(Target::Ps4_9),
        "ps4_10" | "ps4" => Ok(Target::Ps4_10),
        "ps5_1" | "ps5" => Ok(Target::Ps5_1),
        _ => anyhow::bail!("unknown target {s}"),
    }
}

/// Cross-session adaptive bitrate state for one console.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdaptiveRecord {
    pub host_id: String,
    pub base_kbps: u32,
    pub current_kbps: u32,
    #[serde(default)]
    pub clean_sessions: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    hosts: Vec<StoredHost>,
    #[serde(default)]
    adaptive: Vec<AdaptiveRecord>,
}

const CONFIG_DIR_NAME: &str = "gamebridge";
/// Previous product names, newest first. Kept only to migrate registrations.
const LEGACY_CONFIG_DIR_NAMES: [&str; 2] = ["setu", "ps-remoteplay"];

fn resolve_default_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("GAMEBRIDGE_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    // Keep the previous variable working so an existing setup does not appear
    // to lose its registrations.
    if let Some(dir) = std::env::var_os("SETU_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => dirs::config_dir().context("could not determine config directory")?,
    };
    Ok(migrate_in(base))
}

/// Use `base/gamebridge`, copying `hosts.json` from `base/setu` or, failing
/// that, `base/ps-remoteplay` on first run so an existing console registration
/// is not lost. Legacy directories are left untouched and used as a fallback
/// if the copy fails.
fn migrate_in(base: PathBuf) -> PathBuf {
    let dir = base.join(CONFIG_DIR_NAME);
    let new_hosts = dir.join("hosts.json");
    if !new_hosts.exists() {
        for name in LEGACY_CONFIG_DIR_NAMES {
            let legacy = base.join(name);
            let old_hosts = legacy.join("hosts.json");
            if !old_hosts.exists() {
                continue;
            }
            return match copy_legacy_hosts(&old_hosts, &new_hosts) {
                Ok(()) => {
                    eprintln!(
                        "gamebridge: migrated console registrations from {} to {}",
                        old_hosts.display(),
                        new_hosts.display()
                    );
                    dir
                }
                Err(error) => {
                    eprintln!(
                        "gamebridge: could not migrate {} to {}: {error}; using the legacy directory",
                        old_hosts.display(),
                        new_hosts.display()
                    );
                    legacy
                }
            };
        }
    }
    dir
}

fn copy_legacy_hosts(old: &Path, new: &Path) -> Result<()> {
    let data = fs::read(old).with_context(|| format!("failed to read {}", old.display()))?;
    if let Some(parent) = new.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                .with_context(|| format!("failed to secure {}", parent.display()))?;
        }
    }
    fs::write(new, data).with_context(|| format!("failed to write {}", new.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(new, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to secure {}", new.display()))?;
    }
    Ok(())
}

pub struct ConfigStore {
    path: PathBuf,
    inner: Mutex<ConfigFile>,
}

impl ConfigStore {
    pub fn load(explicit_dir: Option<PathBuf>) -> Result<Self> {
        let dir = match explicit_dir {
            Some(dir) => dir,
            None => resolve_default_dir()?,
        };
        Self::load_from_dir(dir)
    }

    fn load_from_dir(dir: PathBuf) -> Result<Self> {
        let path = dir.join("hosts.json");
        let file = match fs::read(&path) {
            Ok(data) => serde_json::from_slice(&data)
                .with_context(|| format!("failed to parse {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => ConfigFile::default(),
            Err(e) => return Err(e).with_context(|| format!("failed to read {}", path.display())),
        };
        Ok(Self {
            path,
            inner: Mutex::new(file),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn hosts(&self) -> Vec<StoredHost> {
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .hosts
            .clone()
    }

    pub fn get(&self, id: &str) -> Option<StoredHost> {
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .hosts
            .iter()
            .find(|h| h.id == id)
            .cloned()
    }

    pub fn add(&self, host: StoredHost) -> Result<()> {
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        inner
            .hosts
            .retain(|h| h.id != host.id && h.host != host.host);
        inner.hosts.push(host);
        self.save(&inner)
    }

    pub fn remove(&self, id: &str) -> Result<bool> {
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        let before = inner.hosts.len();
        inner.hosts.retain(|h| h.id != id);
        let removed = inner.hosts.len() != before;
        let adaptive_before = inner.adaptive.len();
        inner.adaptive.retain(|a| a.host_id != id);
        if removed || inner.adaptive.len() != adaptive_before {
            self.save(&inner)?;
        }
        Ok(removed)
    }

    pub fn adaptive(&self, host_id: &str) -> Option<AdaptiveRecord> {
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .adaptive
            .iter()
            .find(|a| a.host_id == host_id)
            .cloned()
    }

    pub fn set_adaptive(&self, record: AdaptiveRecord) -> Result<()> {
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        inner.adaptive.retain(|a| a.host_id != record.host_id);
        inner.adaptive.push(record);
        self.save(&inner)
    }

    fn save(&self, file: &ConfigFile) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
            }
        }
        let tmp = self.path.with_extension("json.tmp");
        let data = serde_json::to_vec_pretty(file)?;
        fs::write(&tmp, data)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        }
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adaptive_state_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let record = AdaptiveRecord {
            host_id: "host".to_string(),
            base_kbps: 15000,
            current_kbps: 10000,
            clean_sessions: 1,
        };
        {
            let store = ConfigStore::load(Some(dir.path().to_path_buf())).expect("store");
            store.set_adaptive(record.clone()).expect("save");
        }
        let store = ConfigStore::load(Some(dir.path().to_path_buf())).expect("reload");
        assert_eq!(store.adaptive("host"), Some(record));
        assert_eq!(store.adaptive("missing"), None);
    }

    #[test]
    fn migrates_legacy_directory_once() {
        let base = tempfile::tempdir().expect("tempdir");
        let legacy = base.path().join("setu");
        fs::create_dir_all(&legacy).expect("legacy dir");
        fs::write(legacy.join("hosts.json"), br#"{"hosts":[]}"#).expect("legacy hosts");

        let dir = migrate_in(base.path().to_path_buf());
        assert_eq!(dir, base.path().join(CONFIG_DIR_NAME));
        assert_eq!(
            fs::read(dir.join("hosts.json")).expect("migrated hosts"),
            br#"{"hosts":[]}"#
        );

        // A later start must not overwrite the new file with the old one.
        fs::write(dir.join("hosts.json"), br#"{"hosts":[1]}"#).expect("new hosts");
        let again = migrate_in(base.path().to_path_buf());
        assert_eq!(again, dir);
        assert_eq!(
            fs::read(dir.join("hosts.json")).expect("new hosts"),
            br#"{"hosts":[1]}"#
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(dir.join("hosts.json"))
                    .expect("metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(&dir).expect("metadata").permissions().mode() & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn migrates_prefers_setu_then_falls_back_to_ps_remoteplay() {
        let base = tempfile::tempdir().expect("tempdir");
        let older = base.path().join("ps-remoteplay");
        fs::create_dir_all(&older).expect("older dir");
        fs::write(older.join("hosts.json"), br#"{"hosts":[1]}"#).expect("older hosts");
        let newer = base.path().join("setu");
        fs::create_dir_all(&newer).expect("newer dir");
        fs::write(newer.join("hosts.json"), br#"{"hosts":[2]}"#).expect("newer hosts");

        let dir = migrate_in(base.path().to_path_buf());
        assert_eq!(dir, base.path().join(CONFIG_DIR_NAME));
        assert_eq!(
            fs::read(dir.join("hosts.json")).expect("migrated hosts"),
            br#"{"hosts":[2]}"#
        );

        let only_older = tempfile::tempdir().expect("tempdir");
        let legacy = only_older.path().join("ps-remoteplay");
        fs::create_dir_all(&legacy).expect("legacy dir");
        fs::write(legacy.join("hosts.json"), br#"{"hosts":[3]}"#).expect("legacy hosts");
        let dir = migrate_in(only_older.path().to_path_buf());
        assert_eq!(
            fs::read(dir.join("hosts.json")).expect("migrated hosts"),
            br#"{"hosts":[3]}"#
        );
    }
}
