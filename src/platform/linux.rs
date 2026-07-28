use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, ExitStatus};

use anyhow::{Context, Result, bail};
use fs2::FileExt;

use super::DoctorCheck;
use crate::model::Environment;
use crate::store::Store;

pub fn run_in_environment(
    environment: &Environment,
    state_root: &Path,
    command: &[OsString],
) -> Result<i32> {
    if !nix::unistd::Uid::effective().is_root() {
        bail!("entering a full-system sandbox currently requires root privileges");
    }
    if command.is_empty() {
        bail!("no command was provided");
    }
    for tool in ["ip", "nft", "unshare", "mount", "umount", "chroot", "cp"] {
        if !command_exists(tool) {
            bail!("required Linux tool '{tool}' was not found");
        }
    }
    let runtime_lock_path = state_root.join(".runtime.lock");
    let runtime_lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&runtime_lock_path)?;
    runtime_lock
        .lock_exclusive()
        .context("wait for another active sandbox to exit")?;

    let network = NetworkGuard::create(environment)?;
    let executable = std::env::current_exe().context("find current executable")?;
    let mut invocation = Command::new("ip");
    invocation
        .args(["netns", "exec", &network.namespace, "unshare"])
        .args(["--mount", "--pid", "--fork", "--uts", "--ipc"])
        .arg(executable)
        .arg("--state-dir")
        .arg(state_root)
        .arg("__sandbox-init")
        .arg(&environment.name)
        .arg("--")
        .args(command);
    let status = invocation
        .status()
        .context("start isolated Linux environment")?;
    drop(network);
    drop(runtime_lock);
    Ok(exit_code(status))
}

pub fn sandbox_init(environment: &Environment, store: &Store, command: &[OsString]) -> Result<i32> {
    let base = &environment.base_root;
    reject_overlay_delimiters(base)?;
    let environment_dir = store.environment_dir(&environment.name);
    let persistent_upper = store.upper_dir(&environment.name);
    let control = environment_dir.join("control");
    let runtime = environment_dir.join(format!("runtime-{}", std::process::id()));
    let stage_upper = runtime.join("upper");
    let stage_work = runtime.join("work");
    let merged = runtime.join("merged");
    fs::create_dir_all(&runtime)?;
    run_checked(Command::new("mount").args([
        OsStr::new("-t"),
        OsStr::new("tmpfs"),
        OsStr::new("tmpfs"),
        runtime.as_os_str(),
    ]))?;
    let mut mounted_runtime = true;

    let result = (|| -> Result<i32> {
        fs::create_dir_all(&stage_upper)?;
        fs::create_dir_all(&stage_work)?;
        fs::create_dir_all(&merged)?;
        copy_contents(&persistent_upper, &stage_upper)?;

        run_checked(Command::new("mount").arg("--make-rprivate").arg("/"))?;
        let options = format!(
            "lowerdir={},upperdir={},workdir={}",
            base.display(),
            stage_upper.display(),
            stage_work.display()
        );
        run_checked(
            Command::new("mount")
                .args(["-t", "overlay", "overlay", "-o"])
                .arg(options)
                .arg(&merged),
        )?;

        prepare_virtual_mounts(&merged, &control)?;
        let sandbox_command = prepare_control_binary(&control, command)?;
        let inherited_path = std::env::var("PATH").unwrap_or_else(|_| {
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into()
        });
        let status = Command::new("chroot")
            .arg(&merged)
            .args(&sandbox_command)
            .env("NETSANDBOX_ACTIVE", &environment.name)
            .env("NETSANDBOX_SUPERVISED", "1")
            .env("NETSANDBOX_CONTROL", "/run/netsandbox-control")
            .env("NETSANDBOX_BIN", "/run/netsandbox-control/netsandbox")
            .env("PATH", format!("/run/netsandbox-control:{inherited_path}"))
            .env(
                "PS1",
                format!(
                    "[nsb:{}] {}",
                    environment.name,
                    std::env::var("PS1").unwrap_or_else(|_| "\\u@\\h:\\w\\$ ".into())
                ),
            )
            .status()
            .with_context(|| format!("execute command inside '{}'", environment.name))?;

        cleanup_virtual_mounts(&merged);
        run_best_effort(Command::new("umount").arg(&merged));
        replace_contents(&stage_upper, &persistent_upper)?;
        Ok(exit_code(status))
    })();

    if mounted_runtime {
        run_best_effort(Command::new("umount").arg(&runtime));
        mounted_runtime = false;
    }
    let _ = mounted_runtime;
    let _ = fs::remove_dir_all(&runtime);
    result
}

fn prepare_control_binary(control: &Path, command: &[OsString]) -> Result<Vec<OsString>> {
    let executable = std::env::current_exe().context("find netsandbox executable")?;
    let sandbox_binary = control.join("netsandbox");
    fs::copy(&executable, &sandbox_binary).with_context(|| {
        format!(
            "copy {} into the sandbox control directory",
            executable.display()
        )
    })?;
    fs::set_permissions(&sandbox_binary, fs::Permissions::from_mode(0o755))?;
    let mut mapped = command.to_vec();
    if mapped
        .first()
        .is_some_and(|argument| Path::new(argument) == executable)
    {
        mapped[0] = OsString::from("/run/netsandbox-control/netsandbox");
    }
    Ok(mapped)
}

pub fn doctor() -> Vec<DoctorCheck> {
    let mut checks = Vec::new();
    checks.push(DoctorCheck {
        name: "root privileges".into(),
        ok: nix::unistd::Uid::effective().is_root(),
        required: true,
        detail: if nix::unistd::Uid::effective().is_root() {
            "available".into()
        } else {
            "required for the current namespace backend".into()
        },
    });
    checks.push(DoctorCheck {
        name: "OverlayFS".into(),
        ok: fs::read_to_string("/proc/filesystems")
            .map(|contents| contents.contains("overlay"))
            .unwrap_or(false),
        required: true,
        detail: "kernel differential filesystem".into(),
    });
    for tool in ["ip", "nft", "unshare", "mount", "chroot", "cp"] {
        checks.push(DoctorCheck {
            name: tool.into(),
            ok: command_exists(tool),
            required: true,
            detail: "required runtime command".into(),
        });
    }
    let forwarding = fs::read_to_string("/proc/sys/net/ipv4/ip_forward")
        .map(|value| value.trim() == "1")
        .unwrap_or(false);
    checks.push(DoctorCheck {
        name: "IPv4 forwarding".into(),
        ok: forwarding,
        required: true,
        detail: if forwarding {
            "enabled".into()
        } else {
            "will be enabled only while a supervised sandbox is active".into()
        },
    });
    checks
}

fn prepare_virtual_mounts(merged: &Path, control: &Path) -> Result<()> {
    for directory in ["dev", "proc", "sys", "run"] {
        fs::create_dir_all(merged.join(directory))?;
    }
    run_checked(
        Command::new("mount")
            .args(["--rbind", "/dev"])
            .arg(merged.join("dev")),
    )?;
    run_checked(
        Command::new("mount")
            .args(["-t", "proc", "proc"])
            .arg(merged.join("proc")),
    )?;
    run_checked(
        Command::new("mount")
            .args(["-t", "sysfs", "sysfs"])
            .arg(merged.join("sys")),
    )?;
    run_checked(
        Command::new("mount")
            .args(["-t", "tmpfs", "tmpfs"])
            .arg(merged.join("run")),
    )?;
    fs::create_dir_all(control)?;
    fs::create_dir_all(merged.join("run/netsandbox-control"))?;
    run_checked(
        Command::new("mount")
            .arg("--bind")
            .arg(control)
            .arg(merged.join("run/netsandbox-control")),
    )?;
    Ok(())
}

fn cleanup_virtual_mounts(merged: &Path) {
    run_best_effort(
        Command::new("umount")
            .arg("-l")
            .arg(merged.join("run/netsandbox-control")),
    );
    for directory in ["run", "sys", "proc", "dev"] {
        run_best_effort(Command::new("umount").arg("-l").arg(merged.join(directory)));
    }
}

struct NetworkGuard {
    namespace: String,
    host_interface: String,
    nft_table: String,
    restore_forwarding: bool,
}

impl NetworkGuard {
    fn create(environment: &Environment) -> Result<Self> {
        let short = &environment.id.simple().to_string()[..7];
        let namespace = format!("nsb-{short}");
        let host_interface = format!("nsh{short}");
        let guest_interface = format!("nsg{short}");
        let nft_table = format!("nsb_{short}");
        let bytes = environment.id.as_bytes();
        let third = 16 + bytes[0] % 220;
        let fourth = (bytes[1] % 63) * 4;
        let host_address = format!("10.231.{third}.{}", fourth + 1);
        let guest_address = format!("10.231.{third}.{}", fourth + 2);

        run_checked(Command::new("ip").args(["netns", "add", &namespace]))?;
        let mut guard = Self {
            namespace,
            host_interface,
            nft_table,
            restore_forwarding: false,
        };
        let setup = (|| -> Result<()> {
            run_checked(Command::new("ip").args([
                "link",
                "add",
                &guard.host_interface,
                "type",
                "veth",
                "peer",
                "name",
                &guest_interface,
            ]))?;
            run_checked(Command::new("ip").args([
                "link",
                "set",
                &guest_interface,
                "netns",
                &guard.namespace,
            ]))?;
            run_checked(Command::new("ip").args([
                "addr",
                "add",
                &format!("{host_address}/30"),
                "dev",
                &guard.host_interface,
            ]))?;
            run_checked(Command::new("ip").args(["link", "set", &guard.host_interface, "up"]))?;
            run_checked(Command::new("ip").args([
                "-n",
                &guard.namespace,
                "addr",
                "add",
                &format!("{guest_address}/30"),
                "dev",
                &guest_interface,
            ]))?;
            run_checked(Command::new("ip").args([
                "-n",
                &guard.namespace,
                "link",
                "set",
                "lo",
                "up",
            ]))?;
            run_checked(Command::new("ip").args([
                "-n",
                &guard.namespace,
                "link",
                "set",
                &guest_interface,
                "up",
            ]))?;
            run_checked(Command::new("ip").args([
                "-n",
                &guard.namespace,
                "route",
                "add",
                "default",
                "via",
                &host_address,
            ]))?;

            let forwarding = fs::read_to_string("/proc/sys/net/ipv4/ip_forward")?;
            if forwarding.trim() != "1" {
                fs::write("/proc/sys/net/ipv4/ip_forward", "1\n")?;
                guard.restore_forwarding = true;
            }

            run_checked(Command::new("nft").args(["add", "table", "ip", &guard.nft_table]))?;
            run_checked(Command::new("nft").args([
                "add",
                "chain",
                "ip",
                &guard.nft_table,
                "postrouting",
                "{",
                "type",
                "nat",
                "hook",
                "postrouting",
                "priority",
                "srcnat",
                ";",
                "policy",
                "accept",
                ";",
                "}",
            ]))?;
            run_checked(Command::new("nft").args([
                "add",
                "rule",
                "ip",
                &guard.nft_table,
                "postrouting",
                "ip",
                "saddr",
                &format!("{guest_address}/32"),
                "masquerade",
            ]))?;
            run_checked(Command::new("nft").args([
                "add",
                "chain",
                "ip",
                &guard.nft_table,
                "forward",
                "{",
                "type",
                "filter",
                "hook",
                "forward",
                "priority",
                "-100",
                ";",
                "policy",
                "accept",
                ";",
                "}",
            ]))?;
            run_checked(Command::new("nft").args([
                "add",
                "rule",
                "ip",
                &guard.nft_table,
                "forward",
                "iifname",
                &guard.host_interface,
                "accept",
            ]))?;
            run_checked(Command::new("nft").args([
                "add",
                "rule",
                "ip",
                &guard.nft_table,
                "forward",
                "oifname",
                &guard.host_interface,
                "ct",
                "state",
                "established,related",
                "accept",
            ]))?;
            Ok(())
        })();
        if let Err(error) = setup {
            drop(guard);
            return Err(error).context("configure isolated network namespace");
        }
        Ok(guard)
    }
}

impl Drop for NetworkGuard {
    fn drop(&mut self) {
        run_best_effort(Command::new("nft").args(["delete", "table", "ip", &self.nft_table]));
        run_best_effort(Command::new("ip").args(["netns", "delete", &self.namespace]));
        run_best_effort(Command::new("ip").args(["link", "delete", &self.host_interface]));
        if self.restore_forwarding {
            let _ = fs::write("/proc/sys/net/ipv4/ip_forward", "0\n");
        }
    }
}

fn copy_contents(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    if fs::read_dir(source)?.next().is_none() {
        return Ok(());
    }
    run_checked(
        Command::new("cp")
            .args(["-a", "--preserve=all"])
            .arg(source.join("."))
            .arg(destination),
    )
}

fn replace_contents(source: &Path, destination: &Path) -> Result<()> {
    for entry in fs::read_dir(destination)? {
        let path = entry?.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_dir() {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_file(path)?;
        }
    }
    copy_contents(source, destination)
}

fn reject_overlay_delimiters(path: &Path) -> Result<()> {
    let value = path.as_os_str().to_string_lossy();
    if value.contains(',') || value.contains(':') {
        bail!("base path cannot contain ',' or ':' for the OverlayFS backend");
    }
    Ok(())
}

fn run_checked(command: &mut Command) -> Result<()> {
    let description = format!("{command:?}");
    let status = command
        .status()
        .with_context(|| format!("execute {description}"))?;
    if !status.success() {
        bail!("{description} exited with {status}");
    }
    Ok(())
}

fn run_best_effort(command: &mut Command) {
    let _ = command.status();
}

fn command_exists(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|directory| directory.join(name).is_file())
    })
}

fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(128)
}
