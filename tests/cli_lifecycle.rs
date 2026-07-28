use std::fs;

use assert_cmd::Command;
use chrono::{Duration as ChronoDuration, Utc};
use netsandbox::store::Store;
use predicates::prelude::*;
use tempfile::tempdir;

fn binary() -> Command {
    Command::new(assert_cmd::cargo::cargo_bin!("netsandbox"))
}

#[test]
fn creates_lists_diffs_applies_and_rolls_back() {
    let state = tempdir().unwrap();
    let base = tempdir().unwrap();
    fs::write(base.path().join("proxy.conf"), "route = old\n").unwrap();

    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "create",
            "proxy-test",
            "--base",
            base.path().to_str().unwrap(),
            "--capture-connections",
            "false",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created environment 'proxy-test'"));

    binary()
        .args(["--state-dir", state.path().to_str().unwrap(), "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("proxy-test"));

    let candidate = state.path().join("candidate.conf");
    fs::write(&candidate, "route = experimental\n").unwrap();
    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "stage",
            "proxy-test",
            "proxy.conf",
            "--from",
            candidate.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Candidate file:"));

    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "diff",
            "proxy-test",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("-route = old"))
        .stdout(predicate::str::contains("+route = experimental"));

    let apply_output = binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "apply",
            "proxy-test",
            "--yes",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        fs::read_to_string(base.path().join("proxy.conf")).unwrap(),
        "route = experimental\n"
    );
    let output = String::from_utf8(apply_output).unwrap();
    let transaction = output
        .lines()
        .find_map(|line| line.strip_prefix("Rollback transaction: "))
        .unwrap();

    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "rollback",
            transaction,
            "--yes",
        ])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(base.path().join("proxy.conf")).unwrap(),
        "route = old\n"
    );
}

#[test]
fn real_apply_refreshes_stale_required_circuits_before_planning() {
    let state = tempdir().unwrap();
    let base = tempdir().unwrap();
    let target = base.path().join("proxy.conf");
    let candidate = state.path().join("candidate.conf");
    fs::write(&target, "old\n").unwrap();
    fs::write(&candidate, "new\n").unwrap();

    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "create",
            "fresh-apply",
            "--base",
            base.path().to_str().unwrap(),
            "--capture-connections",
            "false",
        ])
        .assert()
        .success();
    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "stage",
            "fresh-apply",
            "proxy.conf",
            "--from",
            candidate.to_str().unwrap(),
        ])
        .assert()
        .success();
    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "circuit",
            "add",
            "fresh-apply",
            "control",
            "--",
            "true",
        ])
        .assert()
        .success();
    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "check",
            "fresh-apply",
        ])
        .assert()
        .success();

    let store = Store::open(Some(state.path().to_path_buf())).unwrap();
    let mut environment = store.load_environment("fresh-apply").unwrap();
    environment.baseline[0].last_checked_at = Some(Utc::now() - ChronoDuration::seconds(61));
    store.save_environment(&environment).unwrap();
    drop(store);

    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "apply",
            "fresh-apply",
            "--yes",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Refreshed 1 required circuit immediately before apply",
        ));
    assert_eq!(fs::read_to_string(target).unwrap(), "new\n");
}

#[test]
fn invalid_names_are_rejected() {
    let state = tempdir().unwrap();
    let base = tempdir().unwrap();
    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "create",
            "../escape",
            "--base",
            base.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid environment name"));
}

#[test]
fn stages_applies_and_rolls_back_a_deletion() {
    let state = tempdir().unwrap();
    let base = tempdir().unwrap();
    let target = base.path().join("stale-route.conf");
    fs::write(&target, "stale\n").unwrap();

    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "create",
            "delete-test",
            "--base",
            base.path().to_str().unwrap(),
            "--capture-connections",
            "false",
        ])
        .assert()
        .success();
    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "stage",
            "delete-test",
            "stale-route.conf",
            "--delete",
        ])
        .assert()
        .success();
    let output = binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "apply",
            "delete-test",
            "--yes",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(!target.exists());
    let transaction = String::from_utf8(output)
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("Rollback transaction: "))
        .unwrap()
        .to_owned();

    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "rollback",
            &transaction,
            "--yes",
        ])
        .assert()
        .success();
    assert_eq!(fs::read_to_string(target).unwrap(), "stale\n");
}

#[test]
fn stages_applies_and_rolls_back_a_directory_deletion() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let state = tempdir().unwrap();
    let base = tempdir().unwrap();
    let directory = base.path().join("proxy.d");
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o750)).unwrap();
    fs::write(directory.join("proxy.conf"), "original\n").unwrap();
    symlink("proxy.conf", directory.join("active.conf")).unwrap();

    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "create",
            "delete-directory",
            "--base",
            base.path().to_str().unwrap(),
            "--capture-connections",
            "false",
        ])
        .assert()
        .success();
    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "stage",
            "delete-directory",
            "proxy.d",
            "--delete",
        ])
        .assert()
        .success();
    let output = binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "apply",
            "delete-directory",
            "--yes",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(!directory.exists());
    let transaction = String::from_utf8(output)
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("Rollback transaction: "))
        .unwrap()
        .to_owned();

    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "rollback",
            &transaction,
            "--yes",
        ])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(directory.join("proxy.conf")).unwrap(),
        "original\n"
    );
    assert_eq!(
        fs::read_link(directory.join("active.conf")).unwrap(),
        std::path::PathBuf::from("proxy.conf")
    );
    assert_eq!(
        fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
        0o750
    );
}

#[cfg(target_os = "macos")]
#[test]
fn mac_route_candidate_uses_loopback_without_changing_routes() {
    use std::net::TcpListener;

    let state = tempdir().unwrap();
    let base = tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "create",
            "mac-route",
            "--base",
            base.path().to_str().unwrap(),
            "--capture-connections",
            "false",
        ])
        .assert()
        .success();

    let preview = binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "mac",
            "route-preview",
            "mac-route",
            "127.0.0.1",
            "--interface",
            "lo0",
            "--port",
            &port.to_string(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "The host routing table was not changed",
        ))
        .get_output()
        .stdout
        .clone();
    let candidate = String::from_utf8(preview)
        .unwrap()
        .lines()
        .find_map(|line| {
            line.strip_prefix("Staged macOS route candidate '")
                .and_then(|value| value.strip_suffix("'."))
        })
        .unwrap()
        .to_owned();

    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "apply",
            "mac-route",
            "--dry-run",
        ])
        .assert()
        .code(2)
        .stdout(predicate::str::contains(
            "has no required application-level canary",
        ));

    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "mac",
            "route-canary",
            "mac-route",
            &candidate,
            "synthetic-loopback",
            "--",
            "true",
        ])
        .assert()
        .success();

    let acceptor = std::thread::spawn(move || listener.accept().unwrap());
    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "mac",
            "test",
            "mac-route",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Preserved"))
        .stdout(predicate::str::contains(
            "The host routing table was not changed",
        ));
    acceptor.join().unwrap();

    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "apply",
            "mac-route",
            "--dry-run",
        ])
        .assert()
        .code(2)
        .stdout(predicate::str::contains("Route changes:          1"))
        .stdout(predicate::str::contains("APPLY BLOCKED"));

    let staged = state.path().join("candidate.conf");
    fs::write(&staged, "candidate\n").unwrap();
    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "stage",
            "mac-route",
            "candidate.conf",
            "--from",
            staged.to_str().unwrap(),
        ])
        .assert()
        .success();
    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "apply",
            "mac-route",
            "--dry-run",
            "--trial",
        ])
        .assert()
        .code(2)
        .stdout(predicate::str::contains(
            "guarded route trial cannot include filesystem changes",
        ));
}

#[cfg(target_os = "macos")]
#[test]
fn mac_gateway_candidate_is_deferred_to_guarded_trial() {
    let state = tempdir().unwrap();
    let base = tempdir().unwrap();

    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "create",
            "mac-gateway",
            "--base",
            base.path().to_str().unwrap(),
            "--capture-connections",
            "false",
        ])
        .assert()
        .success();

    let preview = binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "mac",
            "route-preview",
            "mac-gateway",
            "127.0.0.1",
            "--interface",
            "lo0",
            "--gateway",
            "192.0.2.1",
            "--port",
            "9",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let candidate = String::from_utf8(preview)
        .unwrap()
        .lines()
        .find_map(|line| {
            line.strip_prefix("Staged macOS route candidate '")
                .and_then(|value| value.strip_suffix("'."))
        })
        .unwrap()
        .to_owned();

    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "mac",
            "route-canary",
            "mac-gateway",
            &candidate,
            "synthetic-application",
            "--",
            "true",
        ])
        .assert()
        .success();

    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "mac",
            "test",
            "mac-gateway",
        ])
        .assert()
        .code(2)
        .stdout(predicate::str::contains("Unverifiable"))
        .stdout(predicate::str::contains(
            "Exact candidate-route checks require a guarded live trial",
        ));
}

#[cfg(target_os = "macos")]
#[test]
fn mac_native_exec_maps_staged_files_and_blocks_host_writes() {
    let state = tempdir().unwrap();
    let base = tempdir().unwrap();
    let host_config = base.path().join("proxy.conf");
    let forbidden = base.path().join("host-write-must-not-exist");
    fs::write(&host_config, "host\n").unwrap();

    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "create",
            "mac-native",
            "--base",
            base.path().to_str().unwrap(),
            "--capture-connections",
            "false",
        ])
        .assert()
        .success();
    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "stage",
            "mac-native",
            "proxy.conf",
        ])
        .assert()
        .success();

    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "exec",
            "mac-native",
            "--",
            "/bin/sh",
            "-c",
            "printf 'candidate\\n' > \"$1\"; printf 'forbidden\\n' > \"$2\"",
            "sh",
            host_config.to_str().unwrap(),
            forbidden.to_str().unwrap(),
        ])
        .assert()
        .failure();

    assert_eq!(fs::read_to_string(&host_config).unwrap(), "host\n");
    assert!(!forbidden.exists());
    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "diff",
            "mac-native",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("+candidate"));
}

#[cfg(target_os = "macos")]
#[test]
fn mac_native_exec_allows_the_null_device() {
    let state = tempdir().unwrap();
    let base = tempdir().unwrap();

    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "create",
            "mac-null-device",
            "--base",
            base.path().to_str().unwrap(),
            "--capture-connections",
            "false",
        ])
        .assert()
        .success();

    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "exec",
            "mac-null-device",
            "--",
            "/bin/sh",
            "-c",
            "printf 'probe' >/dev/null",
        ])
        .assert()
        .success();
}

#[cfg(target_os = "macos")]
#[test]
fn mac_native_check_uses_staged_configuration_for_command_canary() {
    let state = tempdir().unwrap();
    let base = tempdir().unwrap();
    let host_config = base.path().join("proxy.conf");
    let candidate = state.path().join("candidate.conf");
    fs::write(&host_config, "host\n").unwrap();
    fs::write(&candidate, "candidate\n").unwrap();

    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "create",
            "mac-canary",
            "--base",
            base.path().to_str().unwrap(),
            "--capture-connections",
            "false",
        ])
        .assert()
        .success();
    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "stage",
            "mac-canary",
            "proxy.conf",
            "--from",
            candidate.to_str().unwrap(),
        ])
        .assert()
        .success();
    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "circuit",
            "add",
            "mac-canary",
            "candidate-config",
            "--",
            "/usr/bin/grep",
            "-q",
            "candidate",
            host_config.to_str().unwrap(),
        ])
        .assert()
        .success();

    binary()
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "check",
            "mac-canary",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Preserved"));
    assert_eq!(fs::read_to_string(host_config).unwrap(), "host\n");
}

#[cfg(target_os = "macos")]
#[test]
fn mac_vm_lifecycle_uses_isolated_runner_and_only_deletes_managed_clones() {
    use std::os::unix::fs::PermissionsExt;

    let state = tempdir().unwrap();
    let base = tempdir().unwrap();
    let fake = tempdir().unwrap();
    let limactl = fake.path().join("limactl");
    let log = fake.path().join("calls.log");
    let guest_root = fake.path().join("guest-root");
    fs::create_dir_all(guest_root.join("etc")).unwrap();
    fs::create_dir_all(base.path().join("etc")).unwrap();
    fs::write(base.path().join("etc/proxy.conf"), "route = host\n").unwrap();
    fs::write(guest_root.join("etc/proxy.conf"), "route = guest\n").unwrap();
    fs::write(
        &limactl,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$FAKE_LIMA_LOG"
case "$1" in
  --version)
    echo "limactl version 2.1.0"
    ;;
  shell)
    case "$*" in
      *"uname -s"*) echo "Darwin" ;;
      *"/usr/bin/test"*)
        for argument in "$@"; do path="$argument"; done
        case "$*" in
          *" -e "*) test -e "$FAKE_LIMA_ROOT$path" ;;
          *" -L "*) test -L "$FAKE_LIMA_ROOT$path" ;;
          *" -d "*) test -d "$FAKE_LIMA_ROOT$path" ;;
          *" -f "*) test -f "$FAKE_LIMA_ROOT$path" ;;
        esac
        exit $?
        ;;
      *"/usr/bin/stat -f %Lp"*)
        echo "600"
        ;;
      *"__probe"*)
        for directory in "$FAKE_LIMA_ROOT"/tmp/netsandbox-*; do
          if test -f "$directory/probe-input.json"; then
            cp "$directory/probe-input.json" "$directory/probe-result.json"
          fi
        done
        ;;
    esac
    ;;
  list)
    echo '{"name":"fake-macos","status":"Running","vmType":"vz"}'
    ;;
  copy)
    source_path="$3"
    target_path="$4"
    case "$source_path" in
      *:*)
        guest_path="${source_path#*:}"
        mkdir -p "$(dirname "$target_path")"
        cp "$FAKE_LIMA_ROOT$guest_path" "$target_path"
        ;;
      *)
        guest_path="${target_path#*:}"
        mkdir -p "$FAKE_LIMA_ROOT$(dirname "$guest_path")"
        cp "$source_path" "$FAKE_LIMA_ROOT$guest_path"
        ;;
    esac
    ;;
esac
exit 0
"#,
    )
    .unwrap();
    fs::set_permissions(&limactl, fs::Permissions::from_mode(0o755)).unwrap();

    for name in ["managed-vm", "attached-vm"] {
        binary()
            .env("NETSANDBOX_LIMACTL", &limactl)
            .env("FAKE_LIMA_LOG", &log)
            .env("FAKE_LIMA_ROOT", &guest_root)
            .args([
                "--state-dir",
                state.path().to_str().unwrap(),
                "create",
                name,
                "--base",
                base.path().to_str().unwrap(),
                "--capture-connections",
                "false",
            ])
            .assert()
            .success();
    }

    binary()
        .env("NETSANDBOX_LIMACTL", &limactl)
        .env("FAKE_LIMA_LOG", &log)
        .env("FAKE_LIMA_ROOT", &guest_root)
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "mac",
            "vm-clone",
            "managed-vm",
            "prepared-base",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Created isolated macOS VM for 'managed-vm'",
        ));
    binary()
        .env("NETSANDBOX_LIMACTL", &limactl)
        .env("FAKE_LIMA_LOG", &log)
        .env("FAKE_LIMA_ROOT", &guest_root)
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "mac",
            "vm-track",
            "managed-vm",
            "/etc/proxy.conf",
        ])
        .assert()
        .success();
    binary()
        .env("NETSANDBOX_LIMACTL", &limactl)
        .env("FAKE_LIMA_LOG", &log)
        .env("FAKE_LIMA_ROOT", &guest_root)
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "enter",
            "managed-vm",
            "--shell",
            "/usr/bin/true",
        ])
        .assert()
        .success();
    binary()
        .env("NETSANDBOX_LIMACTL", &limactl)
        .env("FAKE_LIMA_LOG", &log)
        .env("FAKE_LIMA_ROOT", &guest_root)
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "diff",
            "managed-vm",
            "/etc/proxy.conf",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("-route = host"))
        .stdout(predicate::str::contains("+route = guest"));
    binary()
        .env("NETSANDBOX_LIMACTL", &limactl)
        .env("FAKE_LIMA_LOG", &log)
        .env("FAKE_LIMA_ROOT", &guest_root)
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "check",
            "managed-vm",
        ])
        .assert()
        .success();
    binary()
        .env("NETSANDBOX_LIMACTL", &limactl)
        .env("FAKE_LIMA_LOG", &log)
        .env("FAKE_LIMA_ROOT", &guest_root)
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "mac",
            "vm-reset",
            "managed-vm",
            "--yes",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Recreated the isolated macOS VM for 'managed-vm'",
        ));
    binary()
        .env("NETSANDBOX_LIMACTL", &limactl)
        .env("FAKE_LIMA_LOG", &log)
        .env("FAKE_LIMA_ROOT", &guest_root)
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "changes",
            "managed-vm",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("No filesystem changes"));
    binary()
        .env("NETSANDBOX_LIMACTL", &limactl)
        .env("FAKE_LIMA_LOG", &log)
        .env("FAKE_LIMA_ROOT", &guest_root)
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "discard",
            "managed-vm",
            "--yes",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Removed its managed isolated macOS VM",
        ));
    let managed_calls = fs::read_to_string(&log).unwrap();
    assert!(managed_calls.contains("clone prepared-base nsb-"));
    assert!(managed_calls.contains("shell --start nsb-"));
    assert!(managed_calls.contains("__probe managed-vm"));
    assert!(managed_calls.contains("copy --backend=scp nsb-"));
    assert!(managed_calls.contains("delete --force --tty=false nsb-"));

    fs::write(&log, "").unwrap();
    binary()
        .env("NETSANDBOX_LIMACTL", &limactl)
        .env("FAKE_LIMA_LOG", &log)
        .env("FAKE_LIMA_ROOT", &guest_root)
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "mac",
            "vm-attach",
            "attached-vm",
            "user-owned-vm",
        ])
        .assert()
        .success();
    binary()
        .env("NETSANDBOX_LIMACTL", &limactl)
        .env("FAKE_LIMA_LOG", &log)
        .env("FAKE_LIMA_ROOT", &guest_root)
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "mac",
            "vm-reset",
            "attached-vm",
            "--yes",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("does not own its macOS VM"));
    binary()
        .env("NETSANDBOX_LIMACTL", &limactl)
        .env("FAKE_LIMA_LOG", &log)
        .env("FAKE_LIMA_ROOT", &guest_root)
        .args([
            "--state-dir",
            state.path().to_str().unwrap(),
            "discard",
            "attached-vm",
            "--yes",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("host was not changed"))
        .stdout(predicate::str::contains("Removed its managed").not());
    assert!(!fs::read_to_string(&log).unwrap().contains("delete"));
}
