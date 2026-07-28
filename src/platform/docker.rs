use std::ffi::OsString;
use std::fs;
use std::io::{IsTerminal, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use super::DoctorCheck;
use crate::diff::{inspect_origin, validate_relative_path};
use crate::model::{Environment, LinuxImageChange};
use crate::store::Store;
use anyhow::{Context, Result, bail};
use uuid::Uuid;

const CONTAINER_KEY: &str = "docker_linux_container";
const IMAGE_KEY: &str = "docker_linux_image";
const GUEST_BINARY_KEY: &str = "docker_linux_guest_binary";
const MANAGED_KEY: &str = "docker_linux_managed";
const COMMITTED_IMAGE_KEY: &str = "docker_linux_committed_image";
const COMMITTED_ID_KEY: &str = "docker_linux_committed_id";
const COMMITTED_PARENT_ID_KEY: &str = "docker_linux_committed_parent_id";
const SOURCE_ENTRYPOINT_KEY: &str = "docker_linux_source_entrypoint";
const SOURCE_CMD_KEY: &str = "docker_linux_source_cmd";
const OWNER_LABEL: &str = "dev.netsandbox.environment";

struct GuestInvocation {
    command: Vec<OsString>,
    host_control: PathBuf,
    guest_directory: PathBuf,
}

pub fn has_container(environment: &Environment) -> bool {
    environment.metadata.contains_key(CONTAINER_KEY)
}

pub fn doctor_check() -> DoctorCheck {
    let runtime = find_docker().and_then(|docker| {
        let output = Command::new(&docker)
            .args(["version", "--format", "{{.Server.Os}}/{{.Server.Arch}}"])
            .output()
            .context("inspect Docker engine")?;
        ensure_success(output.status, &output.stderr, "inspect Docker engine")?;
        let platform = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !platform.starts_with("linux/") {
            bail!("Docker engine is not running a Linux kernel");
        }
        Ok((docker, platform))
    });
    DoctorCheck {
        name: "Linux image runner".into(),
        ok: runtime.is_ok(),
        required: false,
        detail: match runtime {
            Ok((path, platform)) => {
                format!("Docker available at {} ({platform})", path.display())
            }
            Err(error) => format!("optional lightweight backend unavailable: {error}"),
        },
    }
}

pub fn discover_guest_binary(store: &Store) -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("NETSANDBOX_LINUX_GUEST_BIN") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        bail!(
            "NETSANDBOX_LINUX_GUEST_BIN points to unavailable file {}",
            path.display()
        );
    }
    let executable = std::env::current_exe().context("find netsandbox executable")?;
    let directory = executable
        .parent()
        .context("netsandbox executable has no parent directory")?;
    for name in [
        "netsandbox-linux-guest",
        match std::env::consts::ARCH {
            "aarch64" => "netsandbox-linux-arm64",
            "x86_64" => "netsandbox-linux-amd64",
            _ => "netsandbox-linux",
        },
    ] {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    #[cfg(netsandbox_embedded_linux_guest)]
    {
        extract_embedded_guest(store)
    }
    #[cfg(not(netsandbox_embedded_linux_guest))]
    {
        let _ = store;
        bail!(
            "this build has no embedded Linux guest helper and none was found beside {}; install 'netsandbox-linux-guest' there or pass --guest-binary",
            executable.display()
        )
    }
}

#[cfg(netsandbox_embedded_linux_guest)]
fn extract_embedded_guest(store: &Store) -> Result<PathBuf> {
    const GUEST: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/netsandbox-linux-guest"));
    let directory = store.root().join("helpers");
    let destination = directory.join("netsandbox-linux-guest");
    if fs::read(&destination).is_ok_and(|existing| existing == GUEST) {
        return Ok(destination);
    }
    fs::create_dir_all(&directory)?;
    let temporary = directory.join(format!(".netsandbox-linux-guest-{}", std::process::id()));
    fs::write(&temporary, GUEST)?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o700))?;
    fs::rename(&temporary, &destination)?;
    Ok(destination)
}

pub fn create(
    environment: &mut Environment,
    store: &Store,
    image: &str,
    guest_binary: &Path,
) -> Result<()> {
    if has_container(environment) {
        bail!(
            "environment '{}' already has a Linux image backend",
            environment.name
        );
    }
    if image.is_empty() || image.starts_with('-') || image.chars().any(char::is_whitespace) {
        bail!("invalid Docker image reference");
    }
    let guest_binary = guest_binary.canonicalize().with_context(|| {
        format!(
            "Linux guest binary {} does not exist",
            guest_binary.display()
        )
    })?;
    if !guest_binary.is_file() {
        bail!(
            "Linux guest binary {} is not a regular file",
            guest_binary.display()
        );
    }
    let docker = find_docker()?;
    let (source_entrypoint, source_cmd) = image_process_config(&docker, image)?;
    let container = expected_container_name(environment);
    if container_exists(&docker, &container)? {
        bail!("Docker container '{container}' already exists");
    }
    start_container(&docker, environment, image, &container, &guest_binary)?;

    let baseline = store
        .environment_dir(&environment.name)
        .join("image-baseline");
    if let Err(error) = fs::create_dir_all(&baseline) {
        let cleanup = remove_container(&docker, &container);
        return match cleanup {
            Ok(()) => Err(error).context("create Linux image baseline directory"),
            Err(cleanup_error) => Err(error).context(format!(
                "create Linux image baseline directory; container cleanup also failed: {cleanup_error:#}"
            )),
        };
    }
    environment.base_root = baseline;
    environment.metadata.insert(CONTAINER_KEY.into(), container);
    environment.metadata.insert(IMAGE_KEY.into(), image.into());
    environment.metadata.insert(
        GUEST_BINARY_KEY.into(),
        guest_binary.to_string_lossy().into_owned(),
    );
    environment
        .metadata
        .insert(MANAGED_KEY.into(), "true".into());
    environment
        .metadata
        .insert(SOURCE_ENTRYPOINT_KEY.into(), source_entrypoint);
    environment
        .metadata
        .insert(SOURCE_CMD_KEY.into(), source_cmd);
    Ok(())
}

pub fn status(environment: &Environment) -> Result<serde_json::Value> {
    let docker = find_docker()?;
    let container = container_name(environment)?;
    let output = Command::new(docker)
        .args(["inspect", container])
        .output()
        .context("inspect Linux image container")?;
    ensure_success(
        output.status,
        &output.stderr,
        "inspect Linux image container",
    )?;
    serde_json::from_slice(&output.stdout).context("parse Docker inspect output")
}

pub fn run_in_environment(
    environment: &Environment,
    state_root: &Path,
    command: &[OsString],
) -> Result<i32> {
    if command.is_empty() {
        bail!("no command was provided");
    }
    let docker = find_docker()?;
    let container = container_name(environment)?;
    ensure_owned_container(&docker, environment, container)?;
    ensure_running(&docker, container)?;
    let guest = prepare_guest_command(&docker, container, environment, state_root, command)?;
    let interactive = command
        .iter()
        .any(|argument| argument == "-i" || argument == "--interactive");

    let mut invocation = Command::new(&docker);
    invocation.arg("exec");
    if interactive {
        invocation.arg("-i");
        if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
            invocation.arg("-t");
        }
    }
    invocation
        .args(["--user", "0"])
        .args(["--env", &format!("NETSANDBOX_ACTIVE={}", environment.name)])
        .args(["--env", &format!("PS1=[nsb:{}] # ", environment.name)])
        .args([
            "--env",
            &format!("NETSANDBOX_CONTROL={}", guest.guest_directory.display()),
        ])
        .args([
            "--env",
            &format!(
                "NETSANDBOX_BIN={}",
                guest.guest_directory.join("netsandbox").display()
            ),
        ])
        .args([
            "--env",
            &format!(
                "PATH={}:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                guest.guest_directory.display()
            ),
        ])
        .arg(container)
        .args(&guest.command);
    let status = invocation
        .status()
        .with_context(|| format!("execute command inside Linux image '{container}'"))?;

    copy_probe_result(
        &docker,
        container,
        &guest.host_control,
        &guest.guest_directory,
    )?;
    remove_guest_control(&docker, container, &guest.guest_directory)?;
    let store = Store::open_unlocked(Some(state_root.to_path_buf()))?;
    let mut current = store.load_environment(&environment.name)?;
    sync_tracked_paths(&mut current, &store)?;
    store.save_environment(&current)?;
    Ok(exit_code(status))
}

pub fn capture_baseline_path(
    environment: &mut Environment,
    store: &Store,
    relative: &Path,
) -> Result<()> {
    validate_relative_path(relative)?;
    let docker = find_docker()?;
    let container = container_name(environment)?;
    ensure_owned_container(&docker, environment, container)?;
    let guest = Path::new("/").join(relative);
    if changes(environment)?
        .iter()
        .any(|change| change.path == guest)
    {
        bail!(
            "{} was already changed in the Linux image; reset the environment, track it, then make the change",
            guest.display()
        );
    }
    let baseline = environment.base_root.join(relative);
    export_regular_file(
        &docker,
        container,
        &guest,
        &baseline,
        "Linux image baseline",
    )?;
    environment
        .origins
        .insert(relative.to_path_buf(), inspect_origin(&baseline)?);
    if !environment.tracked_paths.contains(&relative.to_path_buf()) {
        environment.tracked_paths.push(relative.to_path_buf());
        environment.tracked_paths.sort();
    }
    remove_path(&store.upper_dir(&environment.name).join(relative))?;
    environment
        .deleted_paths
        .retain(|deleted| deleted != relative);
    Ok(())
}

pub fn sync_tracked_paths(environment: &mut Environment, store: &Store) -> Result<usize> {
    let docker = find_docker()?;
    let container = container_name(environment)?.to_owned();
    ensure_owned_container(&docker, environment, &container)?;
    let mut synced = 0;
    for relative in environment.tracked_paths.clone() {
        validate_relative_path(&relative)?;
        let guest = Path::new("/").join(&relative);
        let candidate = store.upper_dir(&environment.name).join(&relative);
        if docker_test(&docker, &container, "-e", &guest)?
            || docker_test(&docker, &container, "-L", &guest)?
        {
            export_regular_file(
                &docker,
                &container,
                &guest,
                &candidate,
                "Linux image candidate",
            )?;
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

pub fn reset(environment: &Environment) -> Result<()> {
    let docker = find_docker()?;
    let container = container_name(environment)?;
    ensure_owned_container(&docker, environment, container)?;
    let image = environment
        .metadata
        .get(IMAGE_KEY)
        .context("Linux image environment has no recorded image")?;
    let guest_binary = guest_binary(environment)?;
    remove_container(&docker, container)?;
    start_container(&docker, environment, image, container, &guest_binary)
        .context("the previous Linux container was deleted, but recreating it failed")
}

pub fn delete_managed(environment: &Environment) -> Result<bool> {
    if environment.metadata.get(MANAGED_KEY).map(String::as_str) != Some("true") {
        return Ok(false);
    }
    let docker = find_docker()?;
    let container = container_name(environment)?;
    if !container_exists(&docker, container)? {
        return Ok(true);
    }
    ensure_owned_container(&docker, environment, container)?;
    remove_container(&docker, container)?;
    Ok(true)
}

pub fn changes(environment: &Environment) -> Result<Vec<LinuxImageChange>> {
    let docker = find_docker()?;
    let container = container_name(environment)?;
    ensure_owned_container(&docker, environment, container)?;
    let output = Command::new(docker)
        .args(["diff", container])
        .output()
        .context("inspect Linux image changes")?;
    ensure_success(output.status, &output.stderr, "inspect Linux image changes")?;
    let mut changes = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some((kind, path)) = line.split_once(' ') else {
            continue;
        };
        let path = PathBuf::from(path);
        if path.starts_with(format!(
            "/tmp/netsandbox-{}",
            &environment.id.simple().to_string()[..12]
        )) {
            continue;
        }
        let relative = path.strip_prefix("/").unwrap_or(&path);
        let tracked = environment
            .tracked_paths
            .iter()
            .any(|allowed| allowed == relative || allowed.starts_with(relative));
        changes.push(LinuxImageChange {
            kind: kind.into(),
            path,
            tracked,
        });
    }
    changes.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(changes)
}

pub fn commit(environment: &mut Environment, output_image: &str) -> Result<String> {
    validate_image_reference(output_image)?;
    let docker = find_docker()?;
    let container = container_name(environment)?;
    ensure_owned_container(&docker, environment, container)?;
    if image_exists(&docker, output_image)? {
        bail!("Docker image '{output_image}' already exists; refusing to overwrite it");
    }
    let untracked = changes(environment)?
        .into_iter()
        .filter(|change| !change.tracked)
        .collect::<Vec<_>>();
    if !untracked.is_empty() {
        bail!(
            "Linux image commit is blocked by {} untracked change(s); run 'netsandbox mac linux-diff {}'",
            untracked.len(),
            environment.name
        );
    }
    let source_entrypoint = environment
        .metadata
        .get(SOURCE_ENTRYPOINT_KEY)
        .context("Linux image environment has no source entrypoint metadata")?;
    let source_cmd = environment
        .metadata
        .get(SOURCE_CMD_KEY)
        .context("Linux image environment has no source command metadata")?;
    let entrypoint = if source_entrypoint == "null" {
        "[]"
    } else {
        source_entrypoint
    };
    let command = if source_cmd == "null" {
        "[]"
    } else {
        source_cmd
    };
    let internal_image = format!(
        "netsandbox-internal:{}-{}",
        &environment.id.simple().to_string()[..12],
        std::process::id()
    );
    if image_exists(&docker, &internal_image)? {
        bail!("temporary Docker image '{internal_image}' already exists");
    }
    let output = Command::new(&docker)
        .args(["commit", container, &internal_image])
        .output()
        .context("commit validated Linux image")?;
    ensure_success(
        output.status,
        &output.stderr,
        "commit validated Linux image",
    )?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parent_id = stdout
        .lines()
        .rev()
        .find(|line| line.trim().starts_with("sha256:"))
        .map(str::trim)
        .context("Docker commit did not return an image ID")?
        .to_owned();
    let dockerfile = format!("FROM {internal_image}\nENTRYPOINT {entrypoint}\nCMD {command}\n");
    let build_result = build_metadata_image(&docker, output_image, &dockerfile);
    let _ = remove_image_reference(&docker, &internal_image);
    build_result?;
    let id = image_id(&docker, output_image)?;
    environment
        .metadata
        .insert(COMMITTED_IMAGE_KEY.into(), output_image.into());
    environment
        .metadata
        .insert(COMMITTED_ID_KEY.into(), id.clone());
    environment
        .metadata
        .insert(COMMITTED_PARENT_ID_KEY.into(), parent_id);
    Ok(id)
}

pub fn rollback_commit(environment: &mut Environment) -> Result<String> {
    let image = environment
        .metadata
        .get(COMMITTED_IMAGE_KEY)
        .context("environment has no committed Linux image")?
        .to_owned();
    let expected_id = environment
        .metadata
        .get(COMMITTED_ID_KEY)
        .context("environment has no committed Linux image ID")?
        .to_owned();
    let docker = find_docker()?;
    let current_id = image_id(&docker, &image)?;
    if current_id != expected_id {
        bail!(
            "refusing to remove Docker image '{image}' because its ID no longer matches the recorded commit"
        );
    }
    let output = Command::new(docker)
        .args(["image", "rm", &image])
        .output()
        .context("remove committed Linux image")?;
    ensure_success(
        output.status,
        &output.stderr,
        "remove committed Linux image",
    )?;
    if let Some(parent) = environment.metadata.get(COMMITTED_PARENT_ID_KEY) {
        let _ = remove_image_reference(&find_docker()?, parent);
    }
    environment.metadata.remove(COMMITTED_IMAGE_KEY);
    environment.metadata.remove(COMMITTED_ID_KEY);
    environment.metadata.remove(COMMITTED_PARENT_ID_KEY);
    Ok(image)
}

fn build_metadata_image(docker: &Path, image: &str, dockerfile: &str) -> Result<()> {
    let context_directory = std::env::temp_dir().join(format!(
        "netsandbox-image-build-{}",
        Uuid::new_v4().simple()
    ));
    fs::create_dir(&context_directory).context("create empty Docker build context")?;
    let result = (|| -> Result<()> {
        let mut child = Command::new(docker)
            .args(["build", "--tag", image, "--file", "-"])
            .arg(&context_directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("start Linux image metadata build")?;
        child
            .stdin
            .as_mut()
            .context("Docker build stdin is unavailable")?
            .write_all(dockerfile.as_bytes())
            .context("write Linux image metadata build")?;
        let output = child
            .wait_with_output()
            .context("wait for Linux image metadata build")?;
        ensure_success(
            output.status,
            &output.stderr,
            "restore Linux source image process metadata",
        )
    })();
    let cleanup = fs::remove_dir(&context_directory).context("remove empty Docker build context");
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup_error)) => Err(error).context(format!(
            "empty Docker build context cleanup also failed: {cleanup_error:#}"
        )),
    }
}

fn start_container(
    docker: &Path,
    environment: &Environment,
    image: &str,
    container: &str,
    guest_binary: &Path,
) -> Result<()> {
    let label = format!("{OWNER_LABEL}={}", environment.id);
    let output = Command::new(docker)
        .args([
            "create",
            "--name",
            container,
            "--label",
            &label,
            "--network",
            "bridge",
            "--cap-add",
            "NET_ADMIN",
            "--cap-add",
            "NET_RAW",
            "--entrypoint",
            "/bin/sh",
            image,
            "-c",
            "trap 'exit 0' TERM INT; while :; do sleep 3600; done",
        ])
        .output()
        .context("create isolated Linux image container")?;
    ensure_success(
        output.status,
        &output.stderr,
        "create isolated Linux image container",
    )?;
    let setup = (|| -> Result<()> {
        ensure_running(docker, container)?;
        let uname = docker_output(docker, container, &["uname", "-s"])?;
        if uname.trim() != "Linux" {
            bail!("Docker image '{image}' is not a Linux image");
        }
        let guest = PathBuf::from("/tmp/netsandbox-bootstrap");
        docker_exec(
            docker,
            container,
            &["/bin/mkdir", "-p", "/tmp/netsandbox-bootstrap"],
        )?;
        docker_copy_to(docker, container, guest_binary, &guest.join("netsandbox"))?;
        docker_exec(
            docker,
            container,
            &["/bin/chmod", "755", "/tmp/netsandbox-bootstrap/netsandbox"],
        )?;
        docker_exec(
            docker,
            container,
            &["/tmp/netsandbox-bootstrap/netsandbox", "--version"],
        )?;
        docker_exec(
            docker,
            container,
            &["/bin/rm", "-rf", "/tmp/netsandbox-bootstrap"],
        )?;
        Ok(())
    })();
    if let Err(error) = setup {
        let _ = remove_container(docker, container);
        return Err(error);
    }
    Ok(())
}

fn prepare_guest_command(
    docker: &Path,
    container: &str,
    environment: &Environment,
    state_root: &Path,
    command: &[OsString],
) -> Result<GuestInvocation> {
    let host_executable = std::env::current_exe().context("find netsandbox executable")?;
    let host_control = state_root
        .join("environments")
        .join(&environment.name)
        .join("control");
    let guest_directory = PathBuf::from(format!(
        "/tmp/netsandbox-{}",
        &environment.id.simple().to_string()[..12]
    ));
    let guest_netsandbox = guest_directory.join("netsandbox");
    docker_exec_os(
        docker,
        container,
        &[
            OsString::from("/bin/mkdir"),
            OsString::from("-p"),
            guest_directory.clone().into_os_string(),
        ],
    )?;
    docker_exec_os(
        docker,
        container,
        &[
            OsString::from("/bin/rm"),
            OsString::from("-f"),
            guest_directory.join("probe-result.json").into_os_string(),
        ],
    )?;
    docker_copy_to(
        docker,
        container,
        &guest_binary(environment)?,
        &guest_netsandbox,
    )?;
    docker_exec_os(
        docker,
        container,
        &[
            OsString::from("/bin/chmod"),
            OsString::from("755"),
            guest_netsandbox.clone().into_os_string(),
        ],
    )?;
    let input = host_control.join("probe-input.json");
    if input.is_file() {
        docker_copy_to(
            docker,
            container,
            &input,
            &guest_directory.join("probe-input.json"),
        )?;
    }
    let mut mapped = command.to_vec();
    if mapped
        .first()
        .is_some_and(|argument| Path::new(argument) == host_executable)
    {
        mapped[0] = guest_netsandbox.into_os_string();
    }
    Ok(GuestInvocation {
        command: mapped,
        host_control,
        guest_directory,
    })
}

fn copy_probe_result(
    docker: &Path,
    container: &str,
    host_control: &Path,
    guest_directory: &Path,
) -> Result<()> {
    let result = guest_directory.join("probe-result.json");
    if docker_test(docker, container, "-f", &result)? {
        fs::create_dir_all(host_control)?;
        docker_copy_from(
            docker,
            container,
            &result,
            &host_control.join("probe-result.json"),
        )?;
    }
    Ok(())
}

fn remove_guest_control(docker: &Path, container: &str, directory: &Path) -> Result<()> {
    docker_exec_os(
        docker,
        container,
        &[
            OsString::from("/bin/rm"),
            OsString::from("-rf"),
            directory.as_os_str().to_owned(),
        ],
    )
}

fn export_regular_file(
    docker: &Path,
    container: &str,
    guest: &Path,
    destination: &Path,
    description: &str,
) -> Result<()> {
    let exists = docker_test(docker, container, "-e", guest)?
        || docker_test(docker, container, "-L", guest)?;
    if !exists {
        remove_path(destination)?;
        return Ok(());
    }
    if docker_test(docker, container, "-L", guest)? {
        bail!(
            "{description} path {} is a symbolic link; symlink export is not yet supported",
            guest.display()
        );
    }
    if docker_test(docker, container, "-d", guest)? {
        bail!(
            "{description} path {} is a directory; track changed files individually",
            guest.display()
        );
    }
    if !docker_test(docker, container, "-f", guest)? {
        bail!(
            "{description} path {} is not a regular file",
            guest.display()
        );
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let mode = docker_file_mode(docker, container, guest)?;
    let temporary =
        destination.with_file_name(format!(".netsandbox-docker-sync-{}", std::process::id()));
    remove_path(&temporary)?;
    if let Err(error) = docker_copy_from(docker, container, guest, &temporary) {
        let _ = remove_path(&temporary);
        return Err(error);
    }
    fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))?;
    remove_path(destination)?;
    fs::rename(&temporary, destination).with_context(|| {
        format!(
            "promote exported Linux image file {}",
            destination.display()
        )
    })
}

fn docker_test(docker: &Path, container: &str, predicate: &str, path: &Path) -> Result<bool> {
    let status = Command::new(docker)
        .args(["exec", "--user", "0", container, "/bin/test", predicate])
        .arg(path)
        .status()
        .with_context(|| format!("inspect {} in Linux image", path.display()))?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!("could not inspect {} in Linux image", path.display()),
    }
}

fn docker_file_mode(docker: &Path, container: &str, path: &Path) -> Result<u32> {
    let output = Command::new(docker)
        .args(["exec", "--user", "0", container, "/bin/stat", "-c", "%a"])
        .arg(path)
        .output()
        .with_context(|| format!("inspect permissions for {} in Linux image", path.display()))?;
    ensure_success(
        output.status,
        &output.stderr,
        "inspect Linux image file permissions",
    )?;
    u32::from_str_radix(String::from_utf8_lossy(&output.stdout).trim(), 8)
        .context("parse Linux image file permissions")
}

fn ensure_running(docker: &Path, container: &str) -> Result<()> {
    let output = Command::new(docker)
        .args(["inspect", "--format", "{{.State.Running}}", container])
        .output()
        .context("inspect Linux image container state")?;
    ensure_success(
        output.status,
        &output.stderr,
        "inspect Linux image container state",
    )?;
    if String::from_utf8_lossy(&output.stdout).trim() == "true" {
        return Ok(());
    }
    let output = Command::new(docker)
        .args(["start", container])
        .output()
        .context("start Linux image container")?;
    ensure_success(output.status, &output.stderr, "start Linux image container")
}

fn ensure_owned_container(docker: &Path, environment: &Environment, container: &str) -> Result<()> {
    if container != expected_container_name(environment) {
        bail!(
            "refusing to operate on Docker container '{container}' because its name does not match environment '{}'",
            environment.name
        );
    }
    let output = Command::new(docker)
        .args([
            "inspect",
            "--format",
            &format!("{{{{ index .Config.Labels \"{OWNER_LABEL}\" }}}}"),
            container,
        ])
        .output()
        .context("inspect Linux image ownership label")?;
    ensure_success(
        output.status,
        &output.stderr,
        "inspect Linux image ownership label",
    )?;
    if String::from_utf8_lossy(&output.stdout).trim() != environment.id.to_string() {
        bail!("Docker container '{container}' is not owned by this environment");
    }
    Ok(())
}

fn remove_container(docker: &Path, container: &str) -> Result<()> {
    let output = Command::new(docker)
        .args(["rm", "--force", container])
        .output()
        .with_context(|| format!("remove Linux image container '{container}'"))?;
    ensure_success(
        output.status,
        &output.stderr,
        "remove Linux image container",
    )
}

fn remove_image_reference(docker: &Path, image: &str) -> Result<()> {
    let output = Command::new(docker)
        .args(["image", "rm", image])
        .output()
        .with_context(|| format!("remove Docker image reference '{image}'"))?;
    ensure_success(
        output.status,
        &output.stderr,
        "remove Docker image reference",
    )
}

fn container_exists(docker: &Path, container: &str) -> Result<bool> {
    let output = Command::new(docker)
        .args(["inspect", container])
        .output()
        .context("inspect Docker container")?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!("could not determine whether Docker container '{container}' exists"),
    }
}

fn image_exists(docker: &Path, image: &str) -> Result<bool> {
    let output = Command::new(docker)
        .args(["image", "inspect", image])
        .output()
        .context("inspect Docker image")?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!("could not determine whether Docker image '{image}' exists"),
    }
}

fn image_process_config(docker: &Path, image: &str) -> Result<(String, String)> {
    let output = Command::new(docker)
        .args([
            "image",
            "inspect",
            "--format",
            "{{json .Config.Entrypoint}}\n{{json .Config.Cmd}}",
            image,
        ])
        .output()
        .context("inspect Linux source image process configuration")?;
    ensure_success(
        output.status,
        &output.stderr,
        "inspect Linux source image process configuration",
    )?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.lines();
    let entrypoint = lines
        .next()
        .context("source image has no entrypoint metadata")?
        .trim()
        .to_owned();
    let command = lines
        .next()
        .context("source image has no command metadata")?
        .trim()
        .to_owned();
    Ok((entrypoint, command))
}

fn image_id(docker: &Path, image: &str) -> Result<String> {
    let output = Command::new(docker)
        .args(["image", "inspect", "--format", "{{.Id}}", image])
        .output()
        .context("inspect committed Linux image")?;
    ensure_success(
        output.status,
        &output.stderr,
        "inspect committed Linux image",
    )?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn docker_output(docker: &Path, container: &str, command: &[&str]) -> Result<String> {
    let output = Command::new(docker)
        .args(["exec", "--user", "0", container])
        .args(command)
        .output()
        .context("execute Linux image inspection command")?;
    ensure_success(
        output.status,
        &output.stderr,
        "execute Linux image inspection command",
    )?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn docker_exec(docker: &Path, container: &str, command: &[&str]) -> Result<()> {
    let status = Command::new(docker)
        .args(["exec", "--user", "0", container])
        .args(command)
        .status()
        .context("execute Linux image setup command")?;
    ensure_success(status, &[], "execute Linux image setup command")
}

fn docker_exec_os(docker: &Path, container: &str, command: &[OsString]) -> Result<()> {
    let status = Command::new(docker)
        .args(["exec", "--user", "0", container])
        .args(command)
        .status()
        .context("execute Linux image setup command")?;
    ensure_success(status, &[], "execute Linux image setup command")
}

fn docker_copy_to(docker: &Path, container: &str, source: &Path, destination: &Path) -> Result<()> {
    let target = format!("{container}:{}", destination.display());
    let output = Command::new(docker)
        .arg("cp")
        .arg(source)
        .arg(target)
        .output()
        .with_context(|| format!("copy {} into Linux image", source.display()))?;
    ensure_success(output.status, &output.stderr, "copy file into Linux image")
}

fn docker_copy_from(
    docker: &Path,
    container: &str,
    source: &Path,
    destination: &Path,
) -> Result<()> {
    let source = format!("{container}:{}", source.display());
    let output = Command::new(docker)
        .arg("cp")
        .arg(source)
        .arg(destination)
        .output()
        .with_context(|| format!("copy {} from Linux image", destination.display()))?;
    ensure_success(output.status, &output.stderr, "copy file from Linux image")
}

fn guest_binary(environment: &Environment) -> Result<PathBuf> {
    let path = environment
        .metadata
        .get(GUEST_BINARY_KEY)
        .map(PathBuf::from)
        .context("Linux image environment has no guest binary")?;
    if !path.is_file() {
        bail!("Linux guest binary {} is unavailable", path.display());
    }
    Ok(path)
}

fn container_name(environment: &Environment) -> Result<&str> {
    environment
        .metadata
        .get(CONTAINER_KEY)
        .map(String::as_str)
        .context("environment has no Linux image runtime")
}

fn expected_container_name(environment: &Environment) -> String {
    format!("nsb-linux-{}", &environment.id.simple().to_string()[..12])
}

fn validate_image_reference(image: &str) -> Result<()> {
    if image.is_empty() || image.starts_with('-') || image.chars().any(char::is_whitespace) {
        bail!("invalid Docker image reference");
    }
    Ok(())
}

fn find_docker() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("NETSANDBOX_DOCKER") {
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
        let candidate = directory.join("docker");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    for candidate in [
        PathBuf::from("/usr/local/bin/docker"),
        PathBuf::from("/opt/homebrew/bin/docker"),
    ] {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!("Docker is required for the lightweight Linux image runner")
}

fn ensure_success(status: ExitStatus, stderr: &[u8], action: &str) -> Result<()> {
    if status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(stderr);
    if detail.trim().is_empty() {
        bail!("{action} failed with status {status}");
    }
    bail!("{action} failed: {}", detail.trim())
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
