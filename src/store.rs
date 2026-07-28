use std::cmp::Reverse;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use fs2::FileExt;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::model::{Environment, Transaction};

pub struct Store {
    root: PathBuf,
    _lock: Option<File>,
}

impl Store {
    pub fn open(override_root: Option<PathBuf>) -> Result<Self> {
        Self::open_with_lock(override_root, true)
    }

    pub(crate) fn open_unlocked(override_root: Option<PathBuf>) -> Result<Self> {
        Self::open_with_lock(override_root, false)
    }

    fn open_with_lock(override_root: Option<PathBuf>, lock_state: bool) -> Result<Self> {
        let root = match override_root {
            Some(path) => path,
            None => {
                let project = ProjectDirs::from("dev", "netsandbox", "NetworkSandbox")
                    .context("could not determine a state directory")?;
                project
                    .state_dir()
                    .unwrap_or_else(|| project.data_local_dir())
                    .to_path_buf()
            }
        };
        fs::create_dir_all(root.join("environments"))
            .with_context(|| format!("create state directory {}", root.display()))?;
        fs::create_dir_all(root.join("transactions"))?;
        let lock = if lock_state {
            let lock_path = root.join(".lock");
            let lock = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&lock_path)
                .with_context(|| format!("open state lock {}", lock_path.display()))?;
            lock.lock_exclusive()
                .context("lock Network Sandbox state")?;
            Some(lock)
        } else {
            None
        };
        Ok(Self { root, _lock: lock })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn environment_dir(&self, name: &str) -> PathBuf {
        self.root.join("environments").join(name)
    }

    pub fn upper_dir(&self, name: &str) -> PathBuf {
        self.environment_dir(name).join("upper")
    }

    pub fn work_dir(&self, name: &str) -> PathBuf {
        self.environment_dir(name).join("work")
    }

    pub fn merged_dir(&self, name: &str) -> PathBuf {
        self.environment_dir(name).join("merged")
    }

    pub fn create_environment(&self, environment: &Environment) -> Result<()> {
        validate_name(&environment.name)?;
        let directory = self.environment_dir(&environment.name);
        if directory.exists() {
            bail!("environment '{}' already exists", environment.name);
        }
        fs::create_dir_all(directory.join("upper"))?;
        fs::create_dir_all(directory.join("work"))?;
        fs::create_dir_all(directory.join("merged"))?;
        self.save_environment(environment)
    }

    pub fn save_environment(&self, environment: &Environment) -> Result<()> {
        write_json_atomic(
            &self
                .environment_dir(&environment.name)
                .join("environment.json"),
            environment,
        )
    }

    pub fn load_environment(&self, name: &str) -> Result<Environment> {
        validate_name(name)?;
        read_json(&self.environment_dir(name).join("environment.json"))
            .with_context(|| format!("environment '{}' does not exist", name))
    }

    pub fn list_environments(&self) -> Result<Vec<Environment>> {
        let mut result = Vec::new();
        for entry in fs::read_dir(self.root.join("environments"))? {
            let entry = entry?;
            let path = entry.path().join("environment.json");
            if path.is_file() {
                result.push(read_json(&path)?);
            }
        }
        result.sort_by(|a: &Environment, b: &Environment| a.name.cmp(&b.name));
        Ok(result)
    }

    pub fn delete_environment(&self, name: &str) -> Result<()> {
        validate_name(name)?;
        let directory = self.environment_dir(name);
        if !directory.exists() {
            bail!("environment '{}' does not exist", name);
        }
        fs::remove_dir_all(&directory)
            .with_context(|| format!("remove environment {}", directory.display()))
    }

    pub fn transaction_dir(&self, id: &str) -> PathBuf {
        self.root.join("transactions").join(id)
    }

    pub fn save_transaction(&self, transaction: &Transaction) -> Result<()> {
        let directory = self.transaction_dir(&transaction.id);
        fs::create_dir_all(directory.join("backup"))?;
        write_json_atomic(&directory.join("transaction.json"), transaction)
    }

    pub fn load_transaction(&self, id: &str) -> Result<Transaction> {
        if !is_safe_identifier(id) {
            bail!("invalid transaction id");
        }
        read_json(&self.transaction_dir(id).join("transaction.json"))
            .with_context(|| format!("transaction '{}' does not exist", id))
    }

    pub fn list_transactions(&self) -> Result<Vec<Transaction>> {
        let mut result: Vec<Transaction> = Vec::new();
        for entry in fs::read_dir(self.root.join("transactions"))? {
            let entry = entry?;
            let path = entry.path().join("transaction.json");
            if path.is_file() {
                result.push(read_json(&path)?);
            }
        }
        result.sort_by_key(|transaction| Reverse(transaction.created_at));
        Ok(result)
    }

    pub fn mark_transaction_committed(&self, id: &str) -> Result<()> {
        if !is_safe_identifier(id) {
            bail!("invalid transaction id");
        }
        let path = self.transaction_dir(id).join("committed");
        let mut file = File::create(&path)
            .with_context(|| format!("create transaction commit marker {}", path.display()))?;
        file.write_all(b"committed\n")?;
        file.sync_all()?;
        Ok(())
    }

    pub fn transaction_is_committed(&self, id: &str) -> Result<bool> {
        if !is_safe_identifier(id) {
            bail!("invalid transaction id");
        }
        Ok(self.transaction_dir(id).join("committed").is_file())
    }

    pub fn transaction_lease_path(&self, id: &str) -> Result<PathBuf> {
        if !is_safe_identifier(id) {
            bail!("invalid transaction id");
        }
        Ok(self.transaction_dir(id).join("rollback.lease"))
    }
}

pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 48
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
        || name.starts_with('.')
    {
        bail!(
            "invalid environment name; use 1-48 letters, numbers, '.', '-' or '_', and do not start with '.'"
        );
    }
    Ok(())
}

fn is_safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_".contains(character))
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path.parent().context("state path has no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    let bytes = serde_json::to_vec_pretty(value)?;
    {
        let mut file = File::create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    fs::rename(&temporary, path)?;
    Ok(())
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_safe_names() {
        for name in ["ssh-test", "proxy_2", "edge.cn", "A1"] {
            validate_name(name).unwrap();
        }
    }

    #[test]
    fn rejects_traversal_names() {
        for name in ["", "../host", ".hidden", "a/b", "with space"] {
            assert!(validate_name(name).is_err(), "{name}");
        }
    }
}
