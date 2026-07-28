use std::collections::BTreeSet;
use std::fs::{self, FileType};
use std::io::Write;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use nix::fcntl::{AT_FDCWD, AtFlags};
use nix::unistd::{Gid, Uid, fchownat};
use uuid::Uuid;

use crate::diff::{conflicts, scan_changes, validate_relative_path};
use crate::model::{
    ApplyPlan, BackupEntry, Change, ChangeKind, Environment, FailurePolicy, ProbeSpec, RouteChange,
    RouteChangeState, STATE_VERSION, Transaction, ValidationState,
};
use crate::platform;
use crate::store::Store;

pub struct RollbackLease {
    stop: Arc<AtomicBool>,
    heartbeat: Option<thread::JoinHandle<()>>,
}

impl RollbackLease {
    fn disabled() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(true)),
            heartbeat: None,
        }
    }

    pub fn commit(mut self, store: &Store, transaction: &str) -> Result<()> {
        store.mark_transaction_committed(transaction)?;
        self.stop()
    }

    pub fn finish(mut self) -> Result<()> {
        self.stop()
    }

    fn stop(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat
                .join()
                .map_err(|_| anyhow::anyhow!("rollback lease heartbeat panicked"))?;
        }
        Ok(())
    }
}

impl Drop for RollbackLease {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

pub fn build_plan(store: &Store, environment: &mut Environment) -> Result<ApplyPlan> {
    build_plan_with_mode(store, environment, false)
}

pub fn build_guarded_trial_plan(store: &Store, environment: &mut Environment) -> Result<ApplyPlan> {
    build_plan_with_mode(store, environment, true)
}

fn build_plan_with_mode(
    store: &Store,
    environment: &mut Environment,
    guarded_trial: bool,
) -> Result<ApplyPlan> {
    let changes = scan_changes(environment, &store.upper_dir(&environment.name))?;
    let conflicts = conflicts(environment, &changes);
    let mut route_conflicts = platform::route_conflicts(&environment.route_candidates)?;
    for candidate in &environment.route_candidates {
        let has_application_canary = environment.baseline.iter().any(|capability| {
            candidate.capability_ids.contains(&capability.id)
                && capability.required
                && matches!(&capability.probe, ProbeSpec::Command { .. })
        });
        if !has_application_canary {
            route_conflicts.push(format!(
                "route candidate {} has no required application-level canary",
                candidate.id
            ));
        }
    }
    if guarded_trial {
        if !cfg!(target_os = "macos") {
            route_conflicts.push("guarded route trials are supported only on macOS".into());
        }
        if environment.route_candidates.is_empty() {
            route_conflicts.push("guarded trial requires a typed macOS route candidate".into());
        }
        if !changes.is_empty() {
            route_conflicts.push(
                "guarded route trial cannot include filesystem changes; apply them in a separate environment"
                    .into(),
            );
        }
        if !environment.policy.auto_rollback {
            route_conflicts.push("guarded trial requires automatic rollback".into());
        }
    }
    let (lost_required, unverifiable_required, deferred_required) =
        classify_validation(environment, guarded_trial);
    let has_validation_failure = !lost_required.is_empty() || !unverifiable_required.is_empty();
    let validation_blocks = if guarded_trial {
        has_validation_failure
    } else {
        environment.policy.on_failure != FailurePolicy::Warn && has_validation_failure
    };
    let has_changes = !changes.is_empty() || !environment.route_candidates.is_empty();
    let allowed =
        has_changes && conflicts.is_empty() && route_conflicts.is_empty() && !validation_blocks;
    Ok(ApplyPlan {
        environment: environment.name.clone(),
        generated_at: Utc::now(),
        changes,
        route_candidates: environment.route_candidates.clone(),
        lost_required,
        unverifiable_required,
        conflicts,
        route_conflicts,
        guarded_trial,
        deferred_required,
        allowed,
    })
}

fn classify_validation(
    environment: &Environment,
    guarded_trial: bool,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let deferred_ids = environment
        .route_candidates
        .iter()
        .flat_map(|candidate| candidate.capability_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
    let deferred_required = if guarded_trial {
        environment
            .baseline
            .iter()
            .filter(|capability| capability.required && deferred_ids.contains(&capability.id))
            .map(|capability| capability.id.clone())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let lost_required = environment
        .baseline
        .iter()
        .filter(|capability| {
            capability.required
                && capability.validation == ValidationState::Lost
                && !(guarded_trial && deferred_ids.contains(&capability.id))
        })
        .map(|capability| capability.id.clone())
        .collect::<Vec<_>>();
    let unverifiable_required = environment
        .baseline
        .iter()
        .filter(|capability| {
            let stale = capability
                .last_checked_at
                .is_none_or(|checked| Utc::now().signed_duration_since(checked).num_seconds() > 60);
            capability.required
                && (stale
                    || matches!(
                        capability.validation,
                        ValidationState::Pending
                            | ValidationState::Unverifiable
                            | ValidationState::Degraded
                    ))
                && !(guarded_trial && deferred_ids.contains(&capability.id))
        })
        .map(|capability| capability.id.clone())
        .collect::<Vec<_>>();
    (lost_required, unverifiable_required, deferred_required)
}

pub fn apply_plan(
    store: &Store,
    environment: &Environment,
    plan: &ApplyPlan,
) -> Result<(Transaction, RollbackLease)> {
    if !plan.allowed {
        bail!("apply plan is blocked");
    }
    let id = format!(
        "apply-{}-{}",
        Utc::now().format("%Y%m%d-%H%M%S"),
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let mut transaction = Transaction {
        version: STATE_VERSION,
        id,
        environment: environment.name.clone(),
        created_at: Utc::now(),
        rolled_back_at: None,
        base_root: environment.base_root.clone(),
        changes: plan.changes.clone(),
        backups: Vec::new(),
        route_changes: plan
            .route_candidates
            .iter()
            .map(|candidate| RouteChange {
                candidate_id: candidate.id.clone(),
                destination: candidate.destination.clone(),
                interface: candidate.interface.clone(),
                gateway: candidate.gateway.clone(),
                original: candidate.observed_route.clone(),
                state: RouteChangeState::Prepared,
            })
            .collect(),
    };
    create_backups(store, &mut transaction)?;
    store.save_transaction(&transaction)?;
    let lease = if environment.policy.auto_rollback {
        arm_rollback_lease(store, &transaction)?
    } else {
        RollbackLease::disabled()
    };

    let upper = store.upper_dir(&environment.name);
    let mut applied = Vec::new();
    for change in &plan.changes {
        if let Err(error) = apply_change(&environment.base_root, &upper, change) {
            let rollback_result = restore_entries(store, &transaction, &applied);
            return match rollback_result {
                Ok(()) => {
                    transaction.rolled_back_at = Some(Utc::now());
                    store.save_transaction(&transaction)?;
                    Err(error).context("apply failed; completed changes were rolled back")
                }
                Err(rollback_error) => Err(error).context(format!(
                    "apply failed and automatic rollback also failed: {rollback_error:#}"
                )),
            };
        }
        applied.push(change.path.clone());
    }

    for index in 0..transaction.route_changes.len() {
        transaction.route_changes[index].state = RouteChangeState::Applying;
        store.save_transaction(&transaction)?;
        if let Err(error) = platform::apply_route_change(&transaction.route_changes[index]) {
            let route_rollback = restore_routes(store, &mut transaction);
            let file_rollback = restore_entries(store, &transaction, &applied);
            return match (route_rollback, file_rollback) {
                (Ok(()), Ok(())) => {
                    transaction.rolled_back_at = Some(Utc::now());
                    store.save_transaction(&transaction)?;
                    Err(error).context("route apply failed; completed changes were rolled back")
                }
                (route_result, file_result) => Err(error).context(format!(
                    "route apply failed and automatic rollback was incomplete; route rollback: {}; file rollback: {}",
                    format_result(route_result),
                    format_result(file_result)
                )),
            };
        }
        transaction.route_changes[index].state = RouteChangeState::Applied;
        store.save_transaction(&transaction)?;
    }
    Ok((transaction, lease))
}

pub fn rollback_transaction(store: &Store, transaction: &mut Transaction) -> Result<()> {
    if transaction.rolled_back_at.is_some() {
        bail!("transaction '{}' was already rolled back", transaction.id);
    }
    restore_routes(store, transaction)?;
    let paths = transaction
        .changes
        .iter()
        .map(|change| change.path.clone())
        .collect::<Vec<_>>();
    restore_entries(store, transaction, &paths)?;
    transaction.rolled_back_at = Some(Utc::now());
    store.save_transaction(transaction)
}

pub fn verify_route_changes(transaction: &Transaction) -> Result<Vec<String>> {
    let mut failures = Vec::new();
    for change in &transaction.route_changes {
        if change.state == RouteChangeState::Applied && !platform::verify_route_change(change)? {
            failures.push(change.destination.clone());
        }
    }
    Ok(failures)
}

fn restore_routes(store: &Store, transaction: &mut Transaction) -> Result<()> {
    for index in (0..transaction.route_changes.len()).rev() {
        if !matches!(
            transaction.route_changes[index].state,
            RouteChangeState::Applying | RouteChangeState::Applied
        ) {
            continue;
        }
        platform::rollback_route_change(&transaction.route_changes[index])?;
        transaction.route_changes[index].state = RouteChangeState::RolledBack;
        store.save_transaction(transaction)?;
    }
    Ok(())
}

fn format_result(result: Result<()>) -> String {
    match result {
        Ok(()) => "ok".into(),
        Err(error) => format!("{error:#}"),
    }
}

fn arm_rollback_lease(store: &Store, transaction: &Transaction) -> Result<RollbackLease> {
    if transaction.base_root != Path::new("/") {
        return Ok(RollbackLease::disabled());
    }

    let lease_path = store.transaction_lease_path(&transaction.id)?;
    write_lease_heartbeat(&lease_path)?;
    let executable = std::env::current_exe().context("find netsandbox executable")?;
    let mut guard = Command::new(executable);
    guard
        .arg("--state-dir")
        .arg(store.root())
        .arg("__rollback-guard")
        .arg(&transaction.id)
        .arg("--timeout")
        .arg("15")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid(2) is async-signal-safe and does not access memory shared with other
        // threads. A new session keeps the rollback guard alive if the invoking terminal exits.
        unsafe {
            guard.pre_exec(|| {
                if nix::libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    guard.spawn().context("start detached rollback guard")?;

    let stop = Arc::new(AtomicBool::new(false));
    let heartbeat_stop = Arc::clone(&stop);
    let heartbeat = thread::spawn(move || {
        while !heartbeat_stop.load(Ordering::Acquire) {
            let _ = write_lease_heartbeat(&lease_path);
            thread::sleep(Duration::from_secs(2));
        }
    });
    Ok(RollbackLease {
        stop,
        heartbeat: Some(heartbeat),
    })
}

fn write_lease_heartbeat(path: &Path) -> Result<()> {
    let mut file = fs::File::create(path)
        .with_context(|| format!("update rollback lease {}", path.display()))?;
    writeln!(file, "{}", Utc::now().timestamp_millis())?;
    file.sync_all()?;
    Ok(())
}

fn create_backups(store: &Store, transaction: &mut Transaction) -> Result<()> {
    let backup_root = store.transaction_dir(&transaction.id).join("backup");
    fs::create_dir_all(&backup_root)?;
    for change in &transaction.changes {
        validate_relative_path(&change.path)?;
        let source = transaction.base_root.join(&change.path);
        match fs::symlink_metadata(&source) {
            Ok(metadata) => {
                let destination = backup_root.join(&change.path);
                copy_entry(&source, &destination)?;
                transaction.backups.push(BackupEntry {
                    path: change.path.clone(),
                    existed: true,
                    kind: Some(file_kind(metadata.file_type()).to_owned()),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                transaction.backups.push(BackupEntry {
                    path: change.path.clone(),
                    existed: false,
                    kind: None,
                });
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn apply_change(base_root: &Path, upper: &Path, change: &Change) -> Result<()> {
    validate_relative_path(&change.path)?;
    let target = base_root.join(&change.path);
    match change.kind {
        ChangeKind::Deleted => remove_path(&target),
        ChangeKind::Added | ChangeKind::Modified | ChangeKind::TypeChanged => {
            copy_entry(&upper.join(&change.path), &target)
        }
    }
}

fn restore_entries(store: &Store, transaction: &Transaction, paths: &[PathBuf]) -> Result<()> {
    let backup_root = store.transaction_dir(&transaction.id).join("backup");
    for path in paths.iter().rev() {
        validate_relative_path(path)?;
        let backup = transaction
            .backups
            .iter()
            .find(|entry| entry.path == *path)
            .with_context(|| format!("missing backup metadata for {}", path.display()))?;
        let target = transaction.base_root.join(path);
        if backup.existed {
            copy_entry(&backup_root.join(path), &target)?;
        } else {
            remove_path(&target)?;
        }
    }
    Ok(())
}

pub(crate) fn copy_entry(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("inspect source {}", source.display()))?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let file_type = metadata.file_type();
    if file_type.is_file() {
        if destination.exists() && !fs::symlink_metadata(destination)?.file_type().is_file() {
            remove_path(destination)?;
        }
        let parent = destination.parent().context("destination has no parent")?;
        let temporary = parent.join(format!(
            ".netsandbox-apply-{}-{}",
            std::process::id(),
            Uuid::new_v4().simple()
        ));
        fs::copy(source, &temporary)?;
        copy_metadata(source, &temporary, &metadata, false)?;
        fs::rename(&temporary, destination)?;
    } else if file_type.is_symlink() {
        remove_path(destination)?;
        symlink(fs::read_link(source)?, destination)?;
        copy_metadata(source, destination, &metadata, true)?;
    } else if file_type.is_dir() {
        if fs::symlink_metadata(destination).is_ok_and(|current| !current.file_type().is_dir()) {
            remove_path(destination)?;
        }
        fs::create_dir_all(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_entry(&entry.path(), &destination.join(entry.file_name()))?;
        }
        copy_metadata(source, destination, &metadata, false)?;
    } else {
        bail!(
            "refusing to apply unsupported special file {}",
            source.display()
        );
    }
    Ok(())
}

fn copy_metadata(
    source: &Path,
    destination: &Path,
    source_metadata: &fs::Metadata,
    no_follow: bool,
) -> Result<()> {
    let destination_metadata = fs::symlink_metadata(destination)?;
    if destination_metadata.uid() != source_metadata.uid()
        || destination_metadata.gid() != source_metadata.gid()
    {
        fchownat(
            AT_FDCWD,
            destination,
            Some(Uid::from_raw(source_metadata.uid())),
            Some(Gid::from_raw(source_metadata.gid())),
            if no_follow {
                AtFlags::AT_SYMLINK_NOFOLLOW
            } else {
                AtFlags::empty()
            },
        )
        .with_context(|| format!("preserve ownership for {}", destination.display()))?;
    }
    for name in xattr::list(source)? {
        if let Some(value) = xattr::get(source, &name)? {
            xattr::set(destination, &name, &value).with_context(|| {
                format!(
                    "preserve extended attribute {:?} for {}",
                    name,
                    destination.display()
                )
            })?;
        }
    }
    if !no_follow {
        fs::set_permissions(
            destination,
            fs::Permissions::from_mode(source_metadata.permissions().mode()),
        )?;
    }
    Ok(())
}

fn remove_path(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            fs::remove_dir_all(path).with_context(|| format!("remove directory {}", path.display()))
        }
        Ok(_) => fs::remove_file(path).with_context(|| format!("remove {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn file_kind(file_type: FileType) -> &'static str {
    if file_type.is_file() {
        "file"
    } else if file_type.is_symlink() {
        "symlink"
    } else if file_type.is_dir() {
        "directory"
    } else if file_type.is_char_device() {
        "char_device"
    } else if file_type.is_block_device() {
        "block_device"
    } else if file_type.is_fifo() {
        "fifo"
    } else if file_type.is_socket() {
        "socket"
    } else {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::model::{
        Capability, Direction, EnvironmentPolicy, ProbeSpec, RouteCandidate, RouteObservation,
        ValidationState,
    };

    #[test]
    fn guarded_trial_defers_only_route_associated_failures() {
        let mut environment = Environment::new(
            "route-trial".into(),
            None,
            PathBuf::from("/"),
            EnvironmentPolicy::default(),
            Vec::new(),
        );
        environment.baseline = vec![
            Capability {
                id: "candidate-ssh".into(),
                name: Some("candidate SSH".into()),
                protocol: "application".into(),
                direction: Direction::Outbound,
                local: "candidate@en0".into(),
                remote: "server".into(),
                process: None,
                probe: ProbeSpec::Command {
                    argv: vec!["true".into()],
                },
                required: true,
                validation: ValidationState::Lost,
                detail: None,
                last_checked_at: Some(Utc::now()),
            },
            Capability {
                id: "codex-control".into(),
                name: Some("Codex control".into()),
                protocol: "tcp".into(),
                direction: Direction::Outbound,
                local: "sandbox".into(),
                remote: "127.0.0.1:443".into(),
                process: None,
                probe: ProbeSpec::Tcp {
                    endpoint: "127.0.0.1:443".into(),
                    interface: None,
                },
                required: true,
                validation: ValidationState::Preserved,
                detail: None,
                last_checked_at: Some(Utc::now()),
            },
        ];
        environment.route_candidates.push(RouteCandidate {
            id: "candidate".into(),
            destination: "203.0.113.10".into(),
            interface: "en0".into(),
            gateway: Some("192.0.2.1".into()),
            ports: vec![22],
            created_at: Utc::now(),
            observed_route: RouteObservation::default(),
            capability_ids: vec!["candidate-ssh".into()],
        });

        let (normal_lost, _, normal_deferred) = classify_validation(&environment, false);
        assert_eq!(normal_lost, ["candidate-ssh"]);
        assert!(normal_deferred.is_empty());

        let (trial_lost, trial_unverifiable, trial_deferred) =
            classify_validation(&environment, true);
        assert!(trial_lost.is_empty());
        assert!(trial_unverifiable.is_empty());
        assert_eq!(trial_deferred, ["candidate-ssh"]);

        environment.baseline[1].validation = ValidationState::Lost;
        let (trial_lost, _, _) = classify_validation(&environment, true);
        assert_eq!(trial_lost, ["codex-control"]);
    }

    #[test]
    fn applies_and_rolls_back_a_file() {
        let state = tempdir().unwrap();
        let root = tempdir().unwrap();
        fs::write(root.path().join("config"), "old\n").unwrap();
        let store = Store::open(Some(state.path().to_path_buf())).unwrap();
        let mut environment = Environment::new(
            "test".into(),
            None,
            root.path().into(),
            EnvironmentPolicy::default(),
            Vec::new(),
        );
        store.create_environment(&environment).unwrap();
        fs::write(store.upper_dir("test").join("config"), "new\n").unwrap();
        let mut plan = build_plan(&store, &mut environment).unwrap();
        assert!(!plan.changes.is_empty());
        plan.allowed = true;
        let (mut transaction, lease) = apply_plan(&store, &environment, &plan).unwrap();
        assert_eq!(
            fs::read_to_string(root.path().join("config")).unwrap(),
            "new\n"
        );
        rollback_transaction(&store, &mut transaction).unwrap();
        lease.finish().unwrap();
        assert_eq!(
            fs::read_to_string(root.path().join("config")).unwrap(),
            "old\n"
        );
        assert!(transaction.rolled_back_at.is_some());
        let _ = ValidationState::Pending;
    }

    #[test]
    fn preserves_mode_and_extended_attributes_across_apply_and_rollback() {
        let state = tempdir().unwrap();
        let root = tempdir().unwrap();
        let target = root.path().join("config");
        fs::write(&target, "old\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
        xattr::set(&target, "user.netsandbox", b"original").unwrap();
        let store = Store::open(Some(state.path().to_path_buf())).unwrap();
        let mut environment = Environment::new(
            "metadata".into(),
            None,
            root.path().into(),
            EnvironmentPolicy::default(),
            Vec::new(),
        );
        store.create_environment(&environment).unwrap();
        let candidate = store.upper_dir("metadata").join("config");
        fs::write(&candidate, "new\n").unwrap();
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o600)).unwrap();
        xattr::set(&candidate, "user.netsandbox", b"candidate").unwrap();
        let mut plan = build_plan(&store, &mut environment).unwrap();
        plan.allowed = true;

        let (mut transaction, lease) = apply_plan(&store, &environment, &plan).unwrap();
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            xattr::get(&target, "user.netsandbox").unwrap().unwrap(),
            b"candidate"
        );

        rollback_transaction(&store, &mut transaction).unwrap();
        lease.finish().unwrap();
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(
            xattr::get(&target, "user.netsandbox").unwrap().unwrap(),
            b"original"
        );
    }
}
