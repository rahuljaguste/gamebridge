// SPDX-License-Identifier: AGPL-3.0-only
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
        let bytes = hex::decode(&self.regist_key).context("invalid regist key")?;
        let mut out = [0u8; 16];
        if bytes.len() != 16 {
            anyhow::bail!("regist key must be 16 bytes");
        }
        out.copy_from_slice(&bytes);
        Ok(out)
    }

    pub fn morning_bytes(&self) -> Result<[u8; 16]> {
        let bytes = hex::decode(&self.morning).context("invalid morning")?;
        let mut out = [0u8; 16];
        if bytes.len() != 16 {
            anyhow::bail!("morning must be 16 bytes");
        }
        out.copy_from_slice(&bytes);
        Ok(out)
    }
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    hosts: Vec<StoredHost>,
}

pub struct ConfigStore {
    path: PathBuf,
    inner: Mutex<ConfigFile>,
}

impl ConfigStore {
    pub fn load(explicit_dir: Option<PathBuf>) -> Result<Self> {
        let dir = explicit_dir
            .or_else(|| std::env::var_os("PS_REMOTEPLAY_CONFIG_DIR").map(PathBuf::from))
            .or_else(|| {
                std::env::var_os("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .map(|p| p.join("ps-remoteplay"))
            })
            .or_else(|| dirs::config_dir().map(|p| p.join("ps-remoteplay")))
            .context("could not determine config directory")?;
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
        self.inner.lock().unwrap().hosts.clone()
    }

    pub fn get(&self, id: &str) -> Option<StoredHost> {
        self.inner
            .lock()
            .unwrap()
            .hosts
            .iter()
            .find(|h| h.id == id)
            .cloned()
    }

    pub fn add(&self, host: StoredHost) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner
            .hosts
            .retain(|h| h.id != host.id && h.host != host.host);
        inner.hosts.push(host);
        self.save(&inner)
    }

    pub fn remove(&self, id: &str) -> Result<bool> {
        let mut inner = self.inner.lock().unwrap();
        let before = inner.hosts.len();
        inner.hosts.retain(|h| h.id != id);
        let removed = inner.hosts.len() != before;
        if removed {
            self.save(&inner)?;
        }
        Ok(removed)
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
