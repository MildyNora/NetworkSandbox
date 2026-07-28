use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use uuid::Uuid;
use walkdir::WalkDir;

use crate::apply::copy_entry;
use crate::diff::validate_relative_path;
use crate::model::Environment;
use crate::store::Store;

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

pub fn available() -> bool {
    Path::new(SANDBOX_EXEC).is_file()
}

pub fn run_in_environment(
    environment: &Environment,
    state_root: &Path,
    command: &[OsString],
) -> Result<i32> {
    if command.is_empty() {
        bail!("no command was provided");
    }
    if !available() {
        bail!("the native macOS rehearsal runtime is unavailable: {SANDBOX_EXEC} is missing");
    }
    if nix::unistd::Uid::effective().is_root() {
        bail!(
            "native macOS rehearsal refuses to execute commands as root; run enter/exec without sudo and reserve sudo for an approved apply"
        );
    }

    let mut workspace = Workspace::create(environment)?;
    let mappings = workspace.materialize_candidates(environment, state_root)?;
    let mapped_command = rewrite_arguments(command, &mappings);
    let profile = sandbox_profile(&workspace.path)?;
    let mapping_json = serde_json::to_string(&mappings)?;

    let mut invocation = Command::new(SANDBOX_EXEC);
    invocation
        .arg("-p")
        .arg(profile)
        .args(&mapped_command)
        .current_dir(&workspace.candidate_root)
        .env("NETSANDBOX_ACTIVE", &environment.name)
        .env("NETSANDBOX_RUNTIME", "macos-native")
        .env("NETSANDBOX_ROOT", &workspace.candidate_root)
        .env("NETSANDBOX_CANDIDATE_ROOT", &workspace.candidate_root)
        .env("NETSANDBOX_CANDIDATE_MAP", mapping_json)
        .env("NETSANDBOX_CONTROL", &workspace.control)
        .env("TMPDIR", &workspace.temporary)
        .env("PS1", format!("[nsb:{}] \\w \\$ ", environment.name));

    let status = invocation
        .status()
        .context("execute command in the native macOS rehearsal runtime")?;

    workspace.export_probe_result(environment, state_root)?;
    workspace.sync_candidates(environment, state_root)?;

    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1)))
}

pub fn rewrite_arguments(
    command: &[OsString],
    mappings: &BTreeMap<String, String>,
) -> Vec<OsString> {
    command
        .iter()
        .map(|argument| {
            let Some(value) = argument.to_str() else {
                return argument.clone();
            };
            if let Some(candidate) = mapped_path(value, mappings) {
                return OsString::from(candidate);
            }
            if let Some((prefix, path)) = value.split_once('=')
                && let Some(candidate) = mapped_path(path, mappings)
            {
                return OsString::from(format!("{prefix}={candidate}"));
            }
            argument.clone()
        })
        .collect()
}

fn mapped_path<'a>(path: &str, mappings: &'a BTreeMap<String, String>) -> Option<&'a String> {
    mappings.get(path).or_else(|| {
        Path::new(path)
            .canonicalize()
            .ok()
            .and_then(|canonical| mappings.get(canonical.to_str()?))
    })
}

fn sandbox_profile(workspace: &Path) -> Result<String> {
    let canonical = workspace
        .canonicalize()
        .context("canonicalize native macOS rehearsal workspace")?;
    let escaped = canonical
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    Ok(format!(
        "(version 1)\
         (deny default)\
         (allow file-read*)\
         (allow file-write-data (literal \"/dev/null\"))\
         (allow process*)\
         (allow signal)\
         (allow sysctl-read)\
         (allow mach-lookup)\
         (allow network-inbound network-outbound)\
         (allow file-write* (subpath \"{escaped}\"))"
    ))
}

struct Workspace {
    path: PathBuf,
    candidate_root: PathBuf,
    control: PathBuf,
    temporary: PathBuf,
    expected: BTreeSet<PathBuf>,
}

impl Workspace {
    fn create(environment: &Environment) -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "netsandbox-native-{}-{}-{}",
            environment.id.simple(),
            std::process::id(),
            Uuid::new_v4().simple()
        ));
        let candidate_root = path.join("candidate");
        let control = path.join("control");
        let temporary = path.join("tmp");
        fs::create_dir_all(&candidate_root)
            .with_context(|| format!("create native workspace {}", candidate_root.display()))?;
        fs::create_dir_all(&control)?;
        fs::create_dir_all(&temporary)?;
        Ok(Self {
            path,
            candidate_root,
            control,
            temporary,
            expected: BTreeSet::new(),
        })
    }

    fn materialize_candidates(
        &mut self,
        environment: &Environment,
        state_root: &Path,
    ) -> Result<BTreeMap<String, String>> {
        self.copy_control_input(environment, state_root)?;
        let upper = state_root
            .join("environments")
            .join(&environment.name)
            .join("upper");
        let mut mappings = BTreeMap::new();

        for relative in environment.origins.keys() {
            validate_relative_path(relative)?;
            self.expected.insert(relative.clone());
            let candidate = self.candidate_root.join(relative);
            if !environment.deleted_paths.contains(relative) {
                let source = upper.join(relative);
                if !source.exists() && !source.is_symlink() {
                    bail!(
                        "staged path /{} has no candidate file; restage it before native execution",
                        relative.display()
                    );
                }
                copy_entry(&source, &candidate)?;
            } else if let Some(parent) = candidate.parent() {
                fs::create_dir_all(parent)?;
            }

            let host = environment.base_root.join(relative);
            mappings.insert(
                host.to_string_lossy().into_owned(),
                candidate.to_string_lossy().into_owned(),
            );
            if environment.base_root == Path::new("/") {
                mappings.insert(
                    format!("/{}", relative.display()),
                    candidate.to_string_lossy().into_owned(),
                );
            }
        }

        Ok(mappings)
    }

    fn copy_control_input(&self, environment: &Environment, state_root: &Path) -> Result<()> {
        let host_input = state_root
            .join("environments")
            .join(&environment.name)
            .join("control/probe-input.json");
        if host_input.is_file() {
            fs::copy(&host_input, self.control.join("probe-input.json"))
                .context("copy connectivity input into native workspace")?;
        }
        Ok(())
    }

    fn export_probe_result(&self, environment: &Environment, state_root: &Path) -> Result<()> {
        let result = self.control.join("probe-result.json");
        if !result.is_file() {
            return Ok(());
        }
        let host_control = state_root
            .join("environments")
            .join(&environment.name)
            .join("control");
        fs::create_dir_all(&host_control)?;
        fs::copy(result, host_control.join("probe-result.json"))
            .context("export native connectivity result")?;
        Ok(())
    }

    fn sync_candidates(&self, environment: &Environment, state_root: &Path) -> Result<()> {
        reject_untracked_entries(&self.candidate_root, &self.expected)?;
        let store = Store::open_unlocked(Some(state_root.to_path_buf()))?;
        let mut current = store.load_environment(&environment.name)?;
        let upper = store.upper_dir(&environment.name);

        for relative in &self.expected {
            validate_relative_path(relative)?;
            let source = self.candidate_root.join(relative);
            let destination = upper.join(relative);
            if source.exists() || source.is_symlink() {
                remove_path(&destination)?;
                copy_entry(&source, &destination)?;
                current.deleted_paths.retain(|deleted| deleted != relative);
            } else {
                remove_path(&destination)?;
                if !current.deleted_paths.contains(relative) {
                    current.deleted_paths.push(relative.clone());
                }
            }
        }
        current.updated_at = Utc::now();
        store.save_environment(&current)
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn reject_untracked_entries(root: &Path, expected: &BTreeSet<PathBuf>) -> Result<()> {
    for entry in WalkDir::new(root).follow_links(false).into_iter() {
        let entry = entry?;
        if entry.path() == root || entry.file_type().is_dir() {
            continue;
        }
        let relative = entry.path().strip_prefix(root)?.to_path_buf();
        if !expected.contains(&relative) {
            bail!(
                "native command created undeclared candidate /{}; stage that exact path before retrying",
                relative.display()
            );
        }
    }
    Ok(())
}

fn remove_path(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            fs::remove_dir_all(path).context("remove staged directory")
        }
        Ok(_) => fs::remove_file(path).context("remove staged file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_exact_and_flag_paths_only() {
        let mappings = BTreeMap::from([(
            "/etc/example.conf".into(),
            "/tmp/candidate/etc/example.conf".into(),
        )]);
        let rewritten = rewrite_arguments(
            &[
                OsString::from("tool"),
                OsString::from("/etc/example.conf"),
                OsString::from("--config=/etc/example.conf"),
                OsString::from("/etc/example.conf.backup"),
            ],
            &mappings,
        );
        assert_eq!(
            rewritten,
            [
                "tool",
                "/tmp/candidate/etc/example.conf",
                "--config=/tmp/candidate/etc/example.conf",
                "/etc/example.conf.backup"
            ]
            .map(OsString::from)
        );
    }
}
