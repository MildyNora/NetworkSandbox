use std::ffi::OsString;

use anyhow::{Result, bail};

use crate::model::{Environment, LinuxImageChange, RouteCandidate, RouteChange};

#[derive(Debug, Clone)]
pub struct DoctorCheck {
    pub name: String,
    pub ok: bool,
    pub required: bool,
    pub detail: String,
}

pub fn run_in_environment(
    environment: &Environment,
    state_root: &std::path::Path,
    command: &[OsString],
) -> Result<i32> {
    #[cfg(target_os = "linux")]
    {
        linux::run_in_environment(environment, state_root, command)
    }
    #[cfg(target_os = "macos")]
    {
        if docker::has_container(environment) {
            docker::run_in_environment(environment, state_root, command)
        } else if macos::has_vm(environment) {
            macos::run_in_environment(environment, state_root, command)
        } else {
            macos_native::run_in_environment(environment, state_root, command)
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (environment, state_root, command);
        bail!("isolated execution is supported only on Linux and macOS")
    }
}

pub fn default_shell(_environment: &Environment) -> OsString {
    #[cfg(target_os = "macos")]
    if docker::has_container(_environment) {
        return OsString::from("/bin/sh");
    }
    std::env::var_os("SHELL").unwrap_or_else(|| OsString::from("/bin/sh"))
}

pub fn doctor() -> Vec<DoctorCheck> {
    #[cfg(target_os = "linux")]
    {
        linux::doctor()
    }
    #[cfg(target_os = "macos")]
    {
        let mut checks = macos::doctor();
        checks.insert(
            0,
            DoctorCheck {
                name: "native enter/exec".into(),
                ok: macos_native::available(),
                required: true,
                detail: if macos_native::available() {
                    "dependency-free differential workspace with write-constrained command execution"
                        .into()
                } else {
                    "/usr/bin/sandbox-exec is unavailable".into()
                },
            },
        );
        let mut legacy = docker::doctor_check();
        legacy.name = "legacy Linux image runner".into();
        legacy.required = false;
        checks.push(legacy);
        checks
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        vec![DoctorCheck {
            name: "Linux kernel".into(),
            ok: false,
            required: true,
            detail: format!("current platform is {}", std::env::consts::OS),
        }]
    }
}

pub fn observe_route(destination: &str) -> Result<crate::model::RouteObservation> {
    #[cfg(target_os = "macos")]
    {
        macos::observe_route(destination)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = destination;
        bail!("macOS route preview is available only on macOS")
    }
}

pub fn validate_interface(interface: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        macos::validate_interface(interface)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = interface;
        bail!("macOS interface validation is available only on macOS")
    }
}

pub fn is_privileged() -> bool {
    #[cfg(unix)]
    {
        nix::unistd::Uid::effective().is_root()
    }
    #[cfg(not(unix))]
    {
        false
    }
}

pub fn require_real_host_apply(environment: &Environment) -> Result<()> {
    if environment.base_root == std::path::Path::new("/") {
        if !cfg!(any(target_os = "linux", target_os = "macos")) {
            bail!("applying to the real host is supported only on Linux and macOS");
        }
        if !is_privileged() {
            bail!("applying to the real host requires root privileges");
        }
    }
    Ok(())
}

pub fn route_conflicts(candidates: &[RouteCandidate]) -> Result<Vec<String>> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    #[cfg(target_os = "macos")]
    {
        macos::route_conflicts(candidates)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = candidates;
        bail!("route candidate application is supported only on macOS")
    }
}

pub fn apply_route_change(change: &RouteChange) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        macos::apply_route_change(change)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = change;
        bail!("route candidate application is supported only on macOS")
    }
}

pub fn rollback_route_change(change: &RouteChange) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        macos::rollback_route_change(change)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = change;
        bail!("route candidate rollback is supported only on macOS")
    }
}

pub fn verify_route_change(change: &RouteChange) -> Result<bool> {
    #[cfg(target_os = "macos")]
    {
        macos::verify_route_change(change)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = change;
        bail!("route candidate verification is supported only on macOS")
    }
}

pub fn has_isolated_runtime(environment: &Environment) -> bool {
    #[cfg(target_os = "linux")]
    {
        let _ = environment;
        true
    }
    #[cfg(target_os = "macos")]
    {
        macos::has_vm(environment)
            || docker::has_container(environment)
            || macos_native::available()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = environment;
        false
    }
}

pub fn has_guest_runtime(environment: &Environment) -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::has_vm(environment) || docker::has_container(environment)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = environment;
        false
    }
}

pub fn is_macos_vm_runtime(environment: &Environment) -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::has_vm(environment)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = environment;
        false
    }
}

pub fn mac_vm_clone(environment: &mut Environment, base_instance: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        if docker::has_container(environment) {
            bail!("environment already has a Linux image runtime");
        }
        macos::vm_clone(environment, base_instance)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (environment, base_instance);
        bail!("the macOS VM backend is available only on macOS")
    }
}

pub fn mac_vm_attach(environment: &mut Environment, instance: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        if docker::has_container(environment) {
            bail!("environment already has a Linux image runtime");
        }
        macos::vm_attach(environment, instance)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (environment, instance);
        bail!("the macOS VM backend is available only on macOS")
    }
}

pub fn mac_vm_status(environment: &Environment) -> Result<serde_json::Value> {
    #[cfg(target_os = "macos")]
    {
        macos::vm_status(environment)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = environment;
        bail!("the macOS VM backend is available only on macOS")
    }
}

pub fn mac_vm_sync(environment: &mut Environment, store: &crate::store::Store) -> Result<usize> {
    #[cfg(target_os = "macos")]
    {
        macos::sync_tracked_paths(environment, store)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (environment, store);
        bail!("the macOS VM backend is available only on macOS")
    }
}

pub fn sync_isolated_runtime(
    environment: &mut Environment,
    store: &crate::store::Store,
) -> Result<usize> {
    #[cfg(target_os = "macos")]
    {
        if docker::has_container(environment) {
            docker::sync_tracked_paths(environment, store)
        } else if macos::has_vm(environment) {
            macos::sync_tracked_paths(environment, store)
        } else {
            Ok(0)
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (environment, store);
        Ok(0)
    }
}

pub fn mac_vm_reset(environment: &Environment) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        macos::reset_managed_vm(environment)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = environment;
        bail!("the macOS VM backend is available only on macOS")
    }
}

pub fn cleanup_environment(environment: &Environment) -> Result<bool> {
    #[cfg(target_os = "macos")]
    {
        if docker::has_container(environment) {
            docker::delete_managed(environment)
        } else {
            macos::delete_managed_vm(environment)
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = environment;
        Ok(false)
    }
}

pub fn is_linux_image_runtime(environment: &Environment) -> bool {
    #[cfg(target_os = "macos")]
    {
        docker::has_container(environment)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = environment;
        false
    }
}

pub fn supports_host_apply(environment: &Environment) -> bool {
    !is_linux_image_runtime(environment)
}

pub fn runtime_description(environment: &Environment) -> Option<&'static str> {
    #[cfg(target_os = "macos")]
    {
        if docker::has_container(environment) {
            Some("lightweight Linux image")
        } else if macos::has_vm(environment) {
            Some("isolated macOS VM")
        } else if macos_native::available() {
            Some("native macOS differential")
        } else {
            None
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = environment;
        None
    }
}

pub fn mac_linux_create(
    environment: &mut Environment,
    store: &crate::store::Store,
    image: &str,
    guest_binary: &std::path::Path,
) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        if macos::has_vm(environment) {
            bail!("environment already has a macOS VM runtime");
        }
        docker::create(environment, store, image, guest_binary)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (environment, store, image, guest_binary);
        bail!("the lightweight Linux image runner is available only on macOS")
    }
}

pub fn mac_linux_guest_binary(store: &crate::store::Store) -> Result<std::path::PathBuf> {
    #[cfg(target_os = "macos")]
    {
        docker::discover_guest_binary(store)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = store;
        bail!("the lightweight Linux image runner is available only on macOS")
    }
}

pub fn mac_linux_status(environment: &Environment) -> Result<serde_json::Value> {
    #[cfg(target_os = "macos")]
    {
        docker::status(environment)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = environment;
        bail!("the lightweight Linux image runner is available only on macOS")
    }
}

pub fn mac_linux_track(
    environment: &mut Environment,
    store: &crate::store::Store,
    relative: &std::path::Path,
) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        docker::capture_baseline_path(environment, store, relative)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (environment, store, relative);
        bail!("the lightweight Linux image runner is available only on macOS")
    }
}

pub fn mac_linux_reset(environment: &Environment) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        docker::reset(environment)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = environment;
        bail!("the lightweight Linux image runner is available only on macOS")
    }
}

pub fn mac_linux_changes(environment: &Environment) -> Result<Vec<LinuxImageChange>> {
    #[cfg(target_os = "macos")]
    {
        docker::changes(environment)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = environment;
        bail!("the lightweight Linux image runner is available only on macOS")
    }
}

pub fn mac_linux_commit(environment: &mut Environment, image: &str) -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        docker::commit(environment, image)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (environment, image);
        bail!("the lightweight Linux image runner is available only on macOS")
    }
}

pub fn mac_linux_rollback(environment: &mut Environment) -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        docker::rollback_commit(environment)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = environment;
        bail!("the lightweight Linux image runner is available only on macOS")
    }
}

#[cfg(target_os = "linux")]
pub(crate) mod linux;

#[cfg(target_os = "macos")]
mod docker;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
mod macos_native;
