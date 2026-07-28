use std::ffi::{CString, OsString};
use std::fs;
use std::net::IpAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use anyhow::{Context, Result, bail};
use nix::libc;

use super::DoctorCheck;
use crate::diff::{inspect_origin, validate_relative_path};
use crate::model::{Environment, RouteCandidate, RouteChange, RouteObservation};
use crate::store::Store;

const ROUTE: &str = "/sbin/route";
const VM_INSTANCE_KEY: &str = "mac_vm_instance";
const VM_MANAGED_KEY: &str = "mac_vm_managed";
const VM_BASE_KEY: &str = "mac_vm_base";

struct GuestInvocation {
    command: Vec<OsString>,
    host_control: PathBuf,
    guest_directory: PathBuf,
}

pub fn has_vm(environment: &Environment) -> bool {
    environment.metadata.contains_key(VM_INSTANCE_KEY)
}

pub fn vm_clone(environment: &mut Environment, base_instance: &str) -> Result<()> {
    validate_vm_name(base_instance)?;
    if has_vm(environment) {
        bail!(
            "environment '{}' already has a macOS VM backend",
            environment.name
        );
    }
    let limactl = find_limactl()?;
    verify_lima_version(&limactl)?;
    verify_macos_guest(&limactl, base_instance)?;
    let instance = format!("nsb-{}", &environment.id.simple().to_string()[..12]);
    clone_macos_vm(&limactl, base_instance, &instance)?;
    environment
        .metadata
        .insert(VM_INSTANCE_KEY.into(), instance);
    environment
        .metadata
        .insert(VM_MANAGED_KEY.into(), "true".into());
    environment
        .metadata
        .insert(VM_BASE_KEY.into(), base_instance.into());
    Ok(())
}

pub fn vm_attach(environment: &mut Environment, instance: &str) -> Result<()> {
    validate_vm_name(instance)?;
    if has_vm(environment) {
        bail!(
            "environment '{}' already has a macOS VM backend",
            environment.name
        );
    }
    let limactl = find_limactl()?;
    verify_lima_version(&limactl)?;
    verify_macos_guest(&limactl, instance)?;
    environment
        .metadata
        .insert(VM_INSTANCE_KEY.into(), instance.into());
    environment
        .metadata
        .insert(VM_MANAGED_KEY.into(), "false".into());
    Ok(())
}

pub fn vm_status(environment: &Environment) -> Result<serde_json::Value> {
    let instance = vm_instance(environment)?;
    let limactl = find_limactl()?;
    let output = Command::new(limactl)
        .args(["list", instance, "--format", "json"])
        .output()
        .context("inspect macOS VM instance")?;
    ensure_success(output.status, &output.stderr, "inspect macOS VM")?;
    serde_json::from_slice(&output.stdout).context("parse Lima instance status")
}

pub fn delete_managed_vm(environment: &Environment) -> Result<bool> {
    if environment.metadata.get(VM_MANAGED_KEY).map(String::as_str) != Some("true") {
        return Ok(false);
    }
    let instance = vm_instance(environment)?;
    let expected = format!("nsb-{}", &environment.id.simple().to_string()[..12]);
    if instance != expected {
        bail!(
            "refusing to delete managed VM '{instance}' because its name does not match environment '{}'",
            environment.name
        );
    }
    let limactl = find_limactl()?;
    verify_lima_version(&limactl)?;
    let output = Command::new(limactl)
        .args(["delete", "--force", "--tty=false", instance])
        .output()
        .with_context(|| format!("delete managed macOS VM '{instance}'"))?;
    ensure_success(output.status, &output.stderr, "delete managed macOS VM")?;
    Ok(true)
}

pub fn reset_managed_vm(environment: &Environment) -> Result<()> {
    if environment.metadata.get(VM_MANAGED_KEY).map(String::as_str) != Some("true") {
        bail!(
            "environment '{}' does not own its macOS VM; reset the attached instance explicitly",
            environment.name
        );
    }
    let instance = vm_instance(environment)?;
    let expected = format!("nsb-{}", &environment.id.simple().to_string()[..12]);
    if instance != expected {
        bail!(
            "refusing to reset managed VM '{instance}' because its name does not match environment '{}'",
            environment.name
        );
    }
    let base = environment
        .metadata
        .get(VM_BASE_KEY)
        .context("managed macOS VM has no recorded base instance")?;
    validate_vm_name(base)?;
    let limactl = find_limactl()?;
    verify_lima_version(&limactl)?;
    verify_macos_guest(&limactl, base)?;
    let deletion = Command::new(&limactl)
        .args(["delete", "--force", "--tty=false", instance])
        .output()
        .with_context(|| format!("delete macOS VM '{instance}' before reset"))?;
    ensure_success(
        deletion.status,
        &deletion.stderr,
        "delete macOS VM before reset",
    )?;
    clone_macos_vm(&limactl, base, instance)
        .context("the previous VM clone was deleted, but recreating it from the base failed")
}

fn clone_macos_vm(limactl: &Path, base: &str, instance: &str) -> Result<()> {
    let output = Command::new(limactl)
        .args(["clone", base, instance, "--mount-none", "--tty=false"])
        .output()
        .context("clone the macOS VM baseline")?;
    ensure_success(output.status, &output.stderr, "clone macOS VM")?;
    if let Err(error) = verify_macos_guest(limactl, instance) {
        let _ = Command::new(limactl)
            .args(["delete", "--force", "--tty=false", instance])
            .status();
        return Err(error).context("start cloned macOS VM");
    }
    Ok(())
}

pub fn run_in_environment(
    environment: &Environment,
    state_root: &Path,
    command: &[OsString],
) -> Result<i32> {
    if command.is_empty() {
        bail!("no command was provided");
    }
    let instance = vm_instance(environment)?;
    let limactl = find_limactl()?;
    verify_lima_version(&limactl)?;
    let interactive = command
        .iter()
        .any(|argument| argument == "-i" || argument == "--interactive");
    let guest = prepare_guest_command(&limactl, instance, environment, state_root, command)?;

    let mut invocation = Command::new(&limactl);
    invocation.arg("shell");
    if !interactive {
        invocation.arg("--tty=false");
    }
    invocation
        .arg("--start")
        .arg(instance)
        .arg("/usr/bin/env")
        .arg(format!("NETSANDBOX_ACTIVE={}", environment.name))
        .arg(format!("PS1=[nsb:{}] \\w \\$ ", environment.name))
        .arg(format!(
            "NETSANDBOX_CONTROL={}",
            guest.guest_directory.display()
        ))
        .arg(format!(
            "NETSANDBOX_BIN={}",
            guest.guest_directory.join("netsandbox").display()
        ))
        .arg(format!(
            "PATH={}:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin",
            guest.guest_directory.display()
        ));
    let status = invocation
        .args(&guest.command)
        .status()
        .with_context(|| format!("execute command inside macOS VM '{instance}'"))?;

    copy_probe_result(
        &limactl,
        instance,
        &guest.host_control,
        &guest.guest_directory,
    )?;
    let store = Store::open_unlocked(Some(state_root.to_path_buf()))?;
    let mut current = store.load_environment(&environment.name)?;
    sync_tracked_paths(&mut current, &store)?;
    store.save_environment(&current)?;
    Ok(exit_code(status))
}

pub fn sync_tracked_paths(environment: &mut Environment, store: &Store) -> Result<usize> {
    let instance = vm_instance(environment)?.to_owned();
    let limactl = find_limactl()?;
    let mut synced = 0;
    for relative in environment.tracked_paths.clone() {
        validate_relative_path(&relative)?;
        let guest = Path::new("/").join(&relative);
        let origin = inspect_origin(&environment.base_root.join(&relative))?;
        environment
            .origins
            .entry(relative.clone())
            .or_insert(origin);
        let exists = guest_test(&limactl, &instance, "-e", &guest)?;
        let symlink = guest_test(&limactl, &instance, "-L", &guest)?;
        let candidate = store.upper_dir(&environment.name).join(&relative);
        if exists || symlink {
            if symlink {
                bail!(
                    "tracked macOS VM path {} is a symbolic link; symlink export is not yet supported",
                    guest.display()
                );
            }
            if guest_test(&limactl, &instance, "-d", &guest)? {
                bail!(
                    "tracked macOS VM path {} is a directory; track its changed files individually",
                    guest.display()
                );
            }
            if !guest_test(&limactl, &instance, "-f", &guest)? {
                bail!(
                    "tracked macOS VM path {} is not a regular file",
                    guest.display()
                );
            }
            if let Some(parent) = candidate.parent() {
                fs::create_dir_all(parent)?;
            }
            let mode = guest_file_mode(&limactl, &instance, &guest)?;
            let temporary = candidate.with_file_name(format!(
                ".netsandbox-vm-sync-{}-{}",
                std::process::id(),
                environment.id.simple()
            ));
            remove_path(&temporary)?;
            let copy_result = copy_from_guest(&limactl, &instance, &guest, &temporary);
            if let Err(error) = copy_result {
                let _ = remove_path(&temporary);
                return Err(error);
            }
            fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))?;
            remove_path(&candidate)?;
            fs::rename(&temporary, &candidate).with_context(|| {
                format!("promote exported macOS VM file {}", candidate.display())
            })?;
            environment
                .deleted_paths
                .retain(|deleted| deleted != &relative);
        } else {
            remove_path(&candidate)?;
            if !environment.deleted_paths.contains(&relative) {
                environment.deleted_paths.push(relative.clone());
            }
        }
        synced += 1;
    }
    Ok(synced)
}

fn guest_test(limactl: &Path, instance: &str, predicate: &str, path: &Path) -> Result<bool> {
    let status = Command::new(limactl)
        .args(["shell", "--tty=false", "--start", instance])
        .arg("/usr/bin/test")
        .arg(predicate)
        .arg(path)
        .status()
        .with_context(|| format!("inspect {} in macOS VM", path.display()))?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!("could not inspect {} in macOS VM", path.display()),
    }
}

fn guest_file_mode(limactl: &Path, instance: &str, path: &Path) -> Result<u32> {
    let output = Command::new(limactl)
        .args([
            "shell",
            "--tty=false",
            "--start",
            instance,
            "/usr/bin/stat",
            "-f",
            "%Lp",
        ])
        .arg(path)
        .output()
        .with_context(|| format!("inspect permissions for {} in macOS VM", path.display()))?;
    ensure_success(
        output.status,
        &output.stderr,
        "inspect macOS VM file permissions",
    )?;
    u32::from_str_radix(String::from_utf8_lossy(&output.stdout).trim(), 8)
        .context("parse macOS VM file permissions")
}

fn vm_instance(environment: &Environment) -> Result<&str> {
    environment
        .metadata
        .get(VM_INSTANCE_KEY)
        .map(String::as_str)
        .with_context(|| {
            format!(
                "environment '{}' has no isolated macOS VM; use 'netsandbox mac vm-clone {} BASE_INSTANCE' or 'vm-attach'",
                environment.name, environment.name
            )
        })
}

fn find_limactl() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("NETSANDBOX_LIMACTL") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
    }
    for directory in std::env::var_os("PATH")
        .as_deref()
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
    {
        let candidate = directory.join("limactl");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    for candidate in [
        PathBuf::from("/opt/homebrew/bin/limactl"),
        PathBuf::from("/usr/local/bin/limactl"),
    ] {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!(
        "Lima 2.1 or newer is required for isolated macOS enter/exec; install it and prepare a macOS base instance"
    )
}

fn verify_lima_version(limactl: &Path) -> Result<()> {
    let output = Command::new(limactl)
        .arg("--version")
        .output()
        .context("inspect Lima version")?;
    ensure_success(output.status, &output.stderr, "inspect Lima version")?;
    require_lima_version(&String::from_utf8_lossy(&output.stdout))
}

fn require_lima_version(text: &str) -> Result<()> {
    let version = text
        .split(|character: char| !character.is_ascii_digit() && character != '.')
        .find(|part| {
            part.split('.')
                .next()
                .is_some_and(|major| !major.is_empty())
        })
        .context("could not parse Lima version")?;
    let mut components = version.split('.');
    let major = components
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let minor = components
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    if (major, minor) < (2, 1) {
        bail!("Lima 2.1 or newer is required; found {}", text.trim());
    }
    Ok(())
}

fn validate_vm_name(instance: &str) -> Result<()> {
    if instance.is_empty()
        || instance.len() > 64
        || !instance
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
    {
        bail!("invalid Lima instance name");
    }
    Ok(())
}

fn verify_macos_guest(limactl: &Path, instance: &str) -> Result<()> {
    let output = Command::new(limactl)
        .args(["shell", "--tty=false", "--start", instance, "uname", "-s"])
        .output()
        .with_context(|| format!("start and inspect Lima instance '{instance}'"))?;
    ensure_success(
        output.status,
        &output.stderr,
        "inspect Lima guest operating system",
    )?;
    if String::from_utf8_lossy(&output.stdout).trim() != "Darwin" {
        bail!("Lima instance '{instance}' is not a macOS guest");
    }
    Ok(())
}

fn prepare_guest_command(
    limactl: &Path,
    instance: &str,
    environment: &Environment,
    state_root: &Path,
    command: &[OsString],
) -> Result<GuestInvocation> {
    let executable = std::env::current_exe().context("find netsandbox executable")?;
    let host_control = state_root
        .join("environments")
        .join(&environment.name)
        .join("control");
    let guest_directory = PathBuf::from(format!(
        "/tmp/netsandbox-{}",
        &environment.id.simple().to_string()[..12]
    ));
    let guest_binary = guest_directory.join("netsandbox");
    let mkdir = Command::new(limactl)
        .args([
            "shell",
            "--tty=false",
            "--start",
            instance,
            "/bin/mkdir",
            "-p",
        ])
        .arg(&guest_directory)
        .output()
        .context("prepare macOS VM probe directory")?;
    ensure_success(
        mkdir.status,
        &mkdir.stderr,
        "prepare macOS VM probe directory",
    )?;
    let clear_result = Command::new(limactl)
        .args(["shell", "--tty=false", "--start", instance, "/bin/rm", "-f"])
        .arg(guest_directory.join("probe-result.json"))
        .output()
        .context("clear stale macOS VM probe result")?;
    ensure_success(
        clear_result.status,
        &clear_result.stderr,
        "clear stale macOS VM probe result",
    )?;
    copy_to_guest(limactl, instance, &executable, &guest_binary)?;
    let chmod = Command::new(limactl)
        .args([
            "shell",
            "--tty=false",
            "--start",
            instance,
            "/bin/chmod",
            "755",
        ])
        .arg(&guest_binary)
        .output()
        .context("make macOS VM probe executable")?;
    ensure_success(
        chmod.status,
        &chmod.stderr,
        "make macOS VM probe executable",
    )?;
    let probe_input = host_control.join("probe-input.json");
    if probe_input.is_file() {
        copy_to_guest(
            limactl,
            instance,
            &probe_input,
            &guest_directory.join("probe-input.json"),
        )?;
    }
    let mut mapped = command.to_vec();
    if command.first().map(PathBuf::from).as_deref() == Some(executable.as_path()) {
        mapped[0] = guest_binary.into_os_string();
    }
    Ok(GuestInvocation {
        command: mapped,
        host_control,
        guest_directory,
    })
}

fn copy_probe_result(
    limactl: &Path,
    instance: &str,
    host_control: &Path,
    guest_directory: &Path,
) -> Result<()> {
    let guest_result = guest_directory.join("probe-result.json");
    let exists = Command::new(limactl)
        .args(["shell", "--tty=false", "--start", instance])
        .arg("/usr/bin/test")
        .arg("-f")
        .arg(&guest_result)
        .status()
        .context("inspect macOS VM probe result")?;
    if exists.success() {
        fs::create_dir_all(host_control)?;
        copy_from_guest(
            limactl,
            instance,
            &guest_result,
            &host_control.join("probe-result.json"),
        )?;
    }
    Ok(())
}

fn copy_to_guest(limactl: &Path, instance: &str, source: &Path, destination: &Path) -> Result<()> {
    let target = format!("{instance}:{}", destination.display());
    let output = Command::new(limactl)
        .args(["copy", "--backend=scp"])
        .arg(source)
        .arg(target)
        .output()
        .with_context(|| format!("copy {} into macOS VM", source.display()))?;
    ensure_success(output.status, &output.stderr, "copy file into macOS VM")
}

fn copy_from_guest(
    limactl: &Path,
    instance: &str,
    source: &Path,
    destination: &Path,
) -> Result<()> {
    let source = format!("{instance}:{}", source.display());
    let output = Command::new(limactl)
        .args(["copy", "--backend=scp"])
        .arg(source)
        .arg(destination)
        .output()
        .with_context(|| format!("copy {} from macOS VM", destination.display()))?;
    ensure_success(output.status, &output.stderr, "copy file from macOS VM")
}

fn ensure_success(status: ExitStatus, stderr: &[u8], action: &str) -> Result<()> {
    if status.success() {
        return Ok(());
    }
    bail!(
        "{action} failed: {}",
        String::from_utf8_lossy(stderr).trim()
    )
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

fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

#[cfg(test)]
mod vm_tests {
    use super::require_lima_version;

    #[test]
    fn accepts_supported_lima_versions() {
        require_lima_version("limactl version 2.1.0").unwrap();
        require_lima_version("limactl version 3.0.1-12-gabc").unwrap();
    }

    #[test]
    fn rejects_old_or_unparseable_lima_versions() {
        assert!(require_lima_version("limactl version 2.0.9").is_err());
        assert!(require_lima_version("development build").is_err());
    }
}

pub fn observe_route(destination: &str) -> Result<RouteObservation> {
    destination
        .parse::<IpAddr>()
        .context("route destination must be an IP address")?;
    let output = Command::new(ROUTE)
        .args(["-n", "get", destination])
        .output()
        .with_context(|| format!("inspect route to {destination}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        bail!("route lookup for {destination} failed: {}", detail.trim());
    }
    parse_route_output(destination, &String::from_utf8_lossy(&output.stdout))
}

pub fn validate_interface(interface: &str) -> Result<()> {
    let name = CString::new(interface).context("invalid interface name")?;
    // SAFETY: `name` is a valid NUL-terminated string for the duration of this call.
    let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
    if index == 0 {
        bail!("network interface '{interface}' does not exist");
    }
    Ok(())
}

pub fn doctor() -> Vec<DoctorCheck> {
    let route_available = std::path::Path::new(ROUTE).is_file();
    let loopback_available = validate_interface("lo0").is_ok();
    let vm_runtime = find_limactl().and_then(|limactl| {
        verify_lima_version(&limactl)?;
        Ok(limactl)
    });
    vec![
        DoctorCheck {
            name: "legacy macOS VM runner".into(),
            ok: vm_runtime.is_ok(),
            required: false,
            detail: match vm_runtime {
                Ok(path) => format!("Lima 2.1+ available at {}", path.display()),
                Err(error) => format!("optional legacy backend unavailable: {error}"),
            },
        },
        DoctorCheck {
            name: "macOS route backend".into(),
            ok: true,
            required: true,
            detail: "interface-bound previews plus transactional route apply/rollback".into(),
        },
        DoctorCheck {
            name: "route inspection".into(),
            ok: route_available,
            required: true,
            detail: "/sbin/route inspection and privileged mutation".into(),
        },
        DoctorCheck {
            name: "IP_BOUND_IF".into(),
            ok: loopback_available,
            required: true,
            detail: "connection-scoped interface binding".into(),
        },
        DoctorCheck {
            name: "differential apply".into(),
            ok: true,
            required: true,
            detail: "filesystem and route changes use backup-backed transactions".into(),
        },
    ]
}

pub fn route_conflicts(candidates: &[RouteCandidate]) -> Result<Vec<String>> {
    let mut destinations = std::collections::BTreeSet::new();
    let mut conflicts = Vec::new();
    for candidate in candidates {
        if !destinations.insert(candidate.destination.clone()) {
            conflicts.push(format!(
                "{} has more than one staged route candidate",
                candidate.destination
            ));
            continue;
        }
        if has_flag(&candidate.observed_route, "HOST")
            && !has_flag(&candidate.observed_route, "STATIC")
        {
            conflicts.push(format!(
                "{} currently uses a non-static host route that cannot be restored exactly",
                candidate.destination
            ));
            continue;
        }
        let current = observe_route(&candidate.destination)?;
        if !routes_equivalent(&current, &candidate.observed_route) {
            conflicts.push(format!(
                "{} changed since candidate {} was staged",
                candidate.destination, candidate.id
            ));
        }
    }
    Ok(conflicts)
}

pub fn apply_route_change(change: &RouteChange) -> Result<()> {
    require_route_privileges()?;
    validate_interface(&change.interface)?;
    let current = observe_route(&change.destination)?;
    if !routes_equivalent(&current, &change.original) {
        bail!(
            "route to {} changed after the apply plan was created",
            change.destination
        );
    }
    if has_flag(&current, "HOST") && !has_flag(&current, "STATIC") {
        bail!(
            "route to {} is a non-static host route and cannot be restored exactly",
            change.destination
        );
    }

    let removed_host_route = has_flag(&current, "HOST");
    if removed_host_route {
        run_route(&delete_host_args(&current))
            .with_context(|| format!("remove the original route to {}", change.destination))?;
    }

    if let Err(error) = run_route(&add_candidate_args(change)) {
        if removed_host_route {
            let _ = restore_original_route(&change.original);
        }
        return Err(error).with_context(|| {
            format!(
                "install route to {} through {}",
                change.destination, change.interface
            )
        });
    }

    if !verify_route_change(change)? {
        let rollback = rollback_route_change(change);
        match rollback {
            Ok(()) => {
                bail!(
                    "installed route to {} did not select {}; the original route was restored",
                    change.destination,
                    change.interface
                )
            }
            Err(rollback_error) => bail!(
                "installed route to {} did not select {}, and rollback failed: {rollback_error:#}",
                change.destination,
                change.interface
            ),
        }
    }
    Ok(())
}

pub fn rollback_route_change(change: &RouteChange) -> Result<()> {
    require_route_privileges()?;
    let current = observe_route(&change.destination)?;
    if routes_equivalent(&current, &change.original) {
        return Ok(());
    }
    if current.interface.as_deref() != Some(change.interface.as_str())
        || !has_flag(&current, "HOST")
    {
        bail!(
            "refusing to overwrite a third-party route change for {}",
            change.destination
        );
    }

    run_route(&delete_host_args(&current))
        .with_context(|| format!("remove candidate route to {}", change.destination))?;
    restore_original_route(&change.original)?;
    let restored = observe_route(&change.destination)?;
    if !routes_equivalent(&restored, &change.original) {
        bail!(
            "route rollback for {} completed but did not reproduce the recorded route",
            change.destination
        );
    }
    Ok(())
}

pub fn verify_route_change(change: &RouteChange) -> Result<bool> {
    let current = observe_route(&change.destination)?;
    let gateway_matches = change
        .gateway
        .as_ref()
        .is_none_or(|gateway| current.gateway.as_ref() == Some(gateway));
    Ok(
        current.interface.as_deref() == Some(change.interface.as_str())
            && gateway_matches
            && has_flag(&current, "HOST"),
    )
}

fn restore_original_route(original: &RouteObservation) -> Result<()> {
    if has_flag(original, "HOST") && has_flag(original, "STATIC") {
        run_route(&restore_route_args(original))
            .with_context(|| format!("restore original route to {}", original.destination))?;
    }
    Ok(())
}

fn require_route_privileges() -> Result<()> {
    if !super::is_privileged() {
        bail!("applying or rolling back a macOS route requires root privileges");
    }
    if !std::path::Path::new(ROUTE).is_file() {
        bail!("{ROUTE} is not available");
    }
    Ok(())
}

fn run_route(arguments: &[String]) -> Result<()> {
    let output = Command::new(ROUTE)
        .args(arguments)
        .output()
        .with_context(|| format!("run {ROUTE} {}", arguments.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        bail!("route command failed: {detail}");
    }
    Ok(())
}

fn family_args(destination: &str) -> Vec<String> {
    if destination
        .parse::<IpAddr>()
        .is_ok_and(|address| address.is_ipv6())
    {
        vec!["-inet6".into()]
    } else {
        Vec::new()
    }
}

fn delete_host_args(route: &RouteObservation) -> Vec<String> {
    let mut arguments = vec!["-n".into(), "delete".into()];
    arguments.extend(family_args(&route.destination));
    arguments.push("-host".into());
    if let Some(ifscope) = &route.ifscope {
        arguments.extend(["-ifscope".into(), ifscope.clone()]);
    }
    arguments.push(route.destination.clone());
    arguments
}

fn add_candidate_args(change: &RouteChange) -> Vec<String> {
    let mut arguments = vec!["-n".into(), "add".into()];
    arguments.extend(family_args(&change.destination));
    arguments.push("-host".into());
    if change.gateway.is_some() {
        arguments.extend(["-ifscope".into(), change.interface.clone()]);
    }
    arguments.push(change.destination.clone());
    if let Some(gateway) = &change.gateway {
        arguments.push(gateway.clone());
    } else {
        arguments.extend(["-interface".into(), change.interface.clone()]);
    }
    arguments
}

fn restore_route_args(route: &RouteObservation) -> Vec<String> {
    let mut arguments = vec!["-n".into(), "add".into()];
    arguments.extend(family_args(&route.destination));
    arguments.push("-host".into());
    if let Some(ifscope) = &route.ifscope {
        arguments.extend(["-ifscope".into(), ifscope.clone()]);
    }
    arguments.push(route.destination.clone());
    match (&route.gateway, &route.interface) {
        (Some(gateway), Some(interface)) if gateway.starts_with("link#") => {
            arguments.extend(["-interface".into(), interface.clone()]);
        }
        (Some(gateway), _) => arguments.push(gateway.clone()),
        (None, Some(interface)) => {
            arguments.extend(["-interface".into(), interface.clone()]);
        }
        (None, None) => {}
    }
    arguments
}

fn routes_equivalent(left: &RouteObservation, right: &RouteObservation) -> bool {
    const STABLE_FLAGS: [&str; 5] = ["HOST", "STATIC", "GATEWAY", "REJECT", "BLACKHOLE"];
    left.destination == right.destination
        && left.gateway == right.gateway
        && left.interface == right.interface
        && left.ifscope == right.ifscope
        && STABLE_FLAGS
            .iter()
            .all(|flag| has_flag(left, flag) == has_flag(right, flag))
}

fn has_flag(route: &RouteObservation, expected: &str) -> bool {
    route.flags.iter().any(|flag| flag == expected)
}

fn parse_route_output(destination: &str, output: &str) -> Result<RouteObservation> {
    let mut observation = RouteObservation {
        destination: destination.into(),
        ..RouteObservation::default()
    };
    for line in output.lines() {
        let Some((key, value)) = line.trim().split_once(':') else {
            continue;
        };
        match key.trim() {
            "destination" => observation.destination = value.trim().into(),
            "gateway" => observation.gateway = Some(value.trim().into()),
            "interface" => observation.interface = Some(value.trim().into()),
            "ifscope" => observation.ifscope = Some(value.trim().into()),
            "flags" => {
                observation.flags = value
                    .trim()
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .split(',')
                    .filter(|flag| !flag.is_empty())
                    .map(ToOwned::to_owned)
                    .collect();
            }
            _ => {}
        }
    }
    Ok(observation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_route_get_output() {
        let route = parse_route_output(
            "8.135.241.49",
            "   route to: 8.135.241.49\n\
             destination: 8.135.241.49\n\
             gateway: 10.3.131.254\n\
             interface: en0\n\
             flags: <UP,GATEWAY,HOST,DONE,STATIC>\n",
        )
        .unwrap();

        assert_eq!(route.gateway.as_deref(), Some("10.3.131.254"));
        assert_eq!(route.interface.as_deref(), Some("en0"));
        assert!(route.flags.contains(&"STATIC".to_owned()));
    }

    #[test]
    fn builds_candidate_and_restore_commands() {
        let change = RouteChange {
            candidate_id: "candidate".into(),
            destination: "8.135.241.49".into(),
            interface: "utun6".into(),
            gateway: None,
            original: RouteObservation {
                destination: "8.135.241.49".into(),
                gateway: Some("10.3.131.254".into()),
                interface: Some("en0".into()),
                ifscope: Some("en0".into()),
                flags: vec![
                    "UP".into(),
                    "GATEWAY".into(),
                    "HOST".into(),
                    "STATIC".into(),
                ],
            },
            state: crate::model::RouteChangeState::Prepared,
        };

        assert_eq!(
            add_candidate_args(&change),
            ["-n", "add", "-host", "8.135.241.49", "-interface", "utun6"]
        );
        assert_eq!(
            restore_route_args(&change.original),
            [
                "-n",
                "add",
                "-host",
                "-ifscope",
                "en0",
                "8.135.241.49",
                "10.3.131.254"
            ]
        );

        let mut gateway_change = change;
        gateway_change.gateway = Some("192.0.2.1".into());
        assert_eq!(
            add_candidate_args(&gateway_change),
            [
                "-n",
                "add",
                "-host",
                "-ifscope",
                "utun6",
                "8.135.241.49",
                "192.0.2.1"
            ]
        );
    }
}
