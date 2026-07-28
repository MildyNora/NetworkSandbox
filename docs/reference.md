# Network Sandbox technical reference

Network Sandbox is a must-have tool and skill that prevent agents terminating the
network when they are handling network related issues like proxies and traffics.
So that they will not kill themselves and leave you alone with those mess.

This is achieved by introducing an isolated sandbox where the agents can test the
validity of the change in configurations before making any change in real environment,
and a workflow to systematically solve network related problems.

On Linux it presents the host filesystem through a copy-on-write OverlayFS
view and runs commands in separate namespaces. On macOS it provides a
dependency-free native differential command runtime, typed route rehearsal,
and transactional host apply/rollback. Legacy Docker and Lima guest runners
remain optional for explicitly selected guest artifacts.

> **Status: functional preview.** The Linux namespace lifecycle has been
> exercised against a real Linux kernel, including isolated mutation,
> transport/application checks, apply, and rollback. The macOS Linux-image
> lifecycle has also been exercised end to end, including source-image
> preservation, allowlisted commit, and exact output-image rollback. Native
> macOS constrained command execution, candidate-aware application canaries,
> file transactions, and route previews are covered by integration tests.
> The remaining boundaries are listed below; do not use this release on a
> production or remotely irreplaceable machine without an independent recovery
> path.

## Install

Download the ready-to-run archive for your platform from the
[main download page](../README.md#download). Building from source is not
required.

Install the bundled Codex skill by linking or copying `skills/network-sandbox`
into `~/.codex/skills/network-sandbox`.

## Development

Rust 1.85 or newer is recommended.

```bash
cargo build --release
cargo test
```

The native Linux runtime requires root and these host facilities:

- OverlayFS
- `ip` from iproute2
- `nft` from nftables
- `unshare`, `mount`, `umount`, and `chroot` from util-linux
- GNU `cp`

Check a Linux host before use:

```bash
sudo netsandbox doctor
```

On macOS, `doctor` verifies the native differential runner, route backend, and
Apple `IP_BOUND_IF` socket support. Docker and Lima are legacy optional guest
backends and are not required.

## Native macOS workflow

The default macOS path uses only the `netsandbox` binary and facilities shipped
with macOS. Stage each writable file, then execute a configuration-aware
command:

```bash
netsandbox create proxy-change
netsandbox stage proxy-change /etc/example-proxy/config

# The absolute staged path is rewritten to an ephemeral candidate path.
netsandbox exec proxy-change -- \
  /usr/local/bin/example-proxy --check-config=/etc/example-proxy/config

netsandbox circuit add proxy-change proxy-canary -- \
  /usr/local/bin/example-proxy-client --config=/etc/example-proxy/config --self-test
netsandbox check proxy-change
netsandbox diff proxy-change
netsandbox plan proxy-change
```

Native commands run unprivileged. They may read host inputs and establish
inbound/outbound connections, but their filesystem writes are constrained to
an ephemeral workspace. Exact staged path arguments and
`--option=/staged/path` values are redirected there. Only previously staged
paths are synchronized back into the differential layer; undeclared output
blocks the run.

For a shadow service, run it in the foreground against the staged
configuration and a temporary port under `netsandbox exec`. While it remains
running, use a second terminal for `netsandbox check` and any required paired
external probe. `enter` exposes `NETSANDBOX_CANDIDATE_ROOT` for interactive
work.

This runtime deliberately does not claim a private macOS root or network
namespace. A command with hard-coded paths, privileged service delegation, or
host-global route/firewall/DNS mutations requires a typed adapter and otherwise
fails closed. Routes use the native route-candidate workflow below.

## Legacy macOS Linux-image workflow

Use this path when the relevant server or appliance can be represented by a
Linux container image. Docker supplies a persistent copy-on-write layer and a
private Linux network stack. The source image and Mac host are never mounted
writable.

Official macOS release builds embed their matching Linux helper in the main
executable. For a development build without an embedded helper, place both
binaries beside one another:

```text
/usr/local/bin/netsandbox
/usr/local/bin/netsandbox-linux-guest
```

Build that self-contained macOS executable with:

```bash
NETSANDBOX_EMBED_LINUX_GUEST=target/docker-linux/release/netsandbox \
  cargo build --release
```

`NETSANDBOX_LINUX_GUEST_BIN` or `--guest-binary PATH` can override embedded or
side-by-side helper discovery. The Linux image must provide `/bin/sh`,
`/bin/test`, `/bin/stat`, `/bin/mkdir`, `/bin/rm`, and `/bin/chmod`.

```bash
netsandbox create proxy-image
netsandbox mac linux-create proxy-image my-proxy:current

# Declare files while the image is still pristine.
netsandbox mac linux-track proxy-image /etc/example-proxy/config

netsandbox circuit add proxy-image api-heartbeat -- \
  /usr/bin/curl --fail --silent https://example.invalid/health
netsandbox exec proxy-image -- /usr/local/bin/change-proxy-config
netsandbox check proxy-image
netsandbox diff proxy-image
netsandbox mac linux-diff proxy-image

# Publish a new image only; this never applies Linux files to the Mac host.
netsandbox mac linux-commit proxy-image my-proxy:validated --yes
```

Commit is blocked by stale/lost required circuits and by every container-layer
change outside the tracked allowlist. It preserves the source image
entrypoint/command and records the exact output image ID. Remove that output
without touching the source:

```bash
netsandbox mac linux-rollback proxy-image --yes
```

Use `netsandbox mac linux-reset proxy-image --yes` to discard the writable
layer and recreate it from the original image. Tracking a path after that path
has already changed is rejected; reset, track, and then mutate.

## Legacy macOS VM workflow

macOS has no native equivalent of Linux network namespaces plus OverlayFS.
Network Sandbox therefore offers an experimental alternative: clone a prepared
Lima macOS guest, run `enter`, `exec`, and `check` inside that VM, and export
only declared regular files into the existing differential/apply engine.

This is genuine VM filesystem and network isolation, but it is not a
copy-on-write projection of the live Mac. The prepared base VM must already
represent the relevant host baseline and contain the commands, applications,
credentials, and privilege setup required by the test.

Requirements:

- Apple silicon macOS;
- Lima 2.1 or newer;
- a prepared macOS base instance, created once with Lima;
- enough disk space for the macOS image and its per-environment clone.

Example:

```bash
# One-time external preparation; this downloads and installs macOS.
limactl start --name netsandbox-base template:macos

netsandbox create proxy-change
netsandbox mac vm-clone proxy-change netsandbox-base

# Declare every regular guest file that may later be promoted.
netsandbox mac vm-track proxy-change /etc/example-proxy/config

netsandbox exec proxy-change -- /path/to/safe-change-command
netsandbox check proxy-change
netsandbox diff proxy-change
netsandbox plan proxy-change
netsandbox apply proxy-change --dry-run

# Only after explicit authorization:
sudo netsandbox apply proxy-change --yes
```

`enter` opens an interactive guest shell. `exec` runs an argument vector
without a host shell. After each command exits, tracked regular files are
copied into the host-side upper layer while preserving their permission bits.
Use `netsandbox mac vm-sync NAME` to export them explicitly.

`netsandbox discard NAME --yes` deletes VM clones created by `vm-clone`. A VM
connected with `vm-attach` remains user-owned and is never deleted by Network
Sandbox.

To throw away all changes in a managed guest and return to its prepared base:

```bash
netsandbox mac vm-reset NAME --yes
```

This recreates only the isolated clone and clears its tracked exports. It
refuses to reset an attached user-owned VM.

Important boundaries:

- only explicitly tracked regular files are eligible for host apply;
- directories, symlinks, special files, ownership, ACLs, and extended
  attributes are not promoted by the experimental runner;
- changes to routes, DNS, firewall, VPN, Network Extension, services, or other
  guest-only state are not inferred as host changes;
- use the native typed route workflow below for a route that may be applied;
- Lima macOS guests disable passwordless `sudo` by default, so non-interactive
  privileged commands require deliberately prepared guest-side authorization.

## macOS route candidate workflow

macOS does not expose Linux-style network namespaces or OverlayFS. Network
Sandbox can preflight interface-bound sockets, but a socket cannot select an
arbitrary candidate host route or gateway. When the staged route differs from
the live route, `mac test` honestly reports its associated checks as
`Unverifiable` and keeps the host unchanged.

```bash
netsandbox create server-route

netsandbox mac route-show SERVER_IP

netsandbox mac route-preview server-route SERVER_IP \
  --interface utun6 \
  --port 22 \
  --port 8443

netsandbox mac route-canary server-route CANDIDATE_ID ssh-check -- \
  /path/to/non-mutating-interface-aware-ssh-check
netsandbox mac test server-route
netsandbox mac route-list server-route
netsandbox apply server-route --dry-run --trial
sudo netsandbox apply server-route --trial --yes
```

Example result:

```text
candidate@utun6 → SERVER_IP:22    Preserved
candidate@utun6 → SERVER_IP:8443  Preserved
The host routing table was not changed.
```

You can compare a second interface without changing the active route:

```bash
netsandbox mac route-preview server-route SERVER_IP \
  --interface en0 \
  --port 22 \
  --port 8443
```

For a gateway route, add `--gateway GATEWAY_IP`. `mac test` revalidates ordinary control circuits
but defers checks that require the not-yet-installed route. Its exit status is therefore expected
to be nonzero for an exact route change. Every route candidate still requires an associated
application canary; successful TCP probes alone never authorize apply.

`apply --trial` resolves the preflight cycle without pretending to emulate a route. It accepts
only typed route changes, requires automatic rollback, rejects failed or stale unrelated control
circuits, and arms a detached rollback lease before the first route mutation. It then installs
the candidate, verifies the selected route, and reruns every required TCP and application check.
It commits only when all checks are `Preserved`; otherwise it immediately restores the original
route. Preview the trial first, and run the real command only with explicit authorization.

Real apply refreshes every required pre-apply circuit immediately after administrator
authorization and before constructing the final plan. The 60-second freshness window therefore
does not include time spent entering a macOS password or granting agent authorization. Do not ask
the operator to race that window or manually repeat `mac test` immediately before apply.

Removing an unapplied candidate removes metadata and probe definitions only:

```bash
netsandbox mac route-remove server-route CANDIDATE_ID
```

Without an attached macOS VM, use `netsandbox stage` for explicit differential
files. Applying staged files and routes to `/` requires root and creates a
rollback transaction.

The native macOS executor permits read access to host files and narrowly permits writes to the
existing `/dev/null` character device, which SSH and other Unix tools commonly require. It does
not expose arbitrary writable devices or treat device nodes as staged files.

## Basic workflow

```bash
sudo netsandbox create proxy-change \
  --description "Test a new proxy configuration"

netsandbox enter proxy-change

[nsb:proxy-change]# edit /etc/example-proxy/config
[nsb:proxy-change]# netsandbox check proxy-change
[nsb:proxy-change]# exit

sudo netsandbox diff proxy-change
sudo netsandbox plan proxy-change
sudo netsandbox apply proxy-change --dry-run
sudo netsandbox apply proxy-change --yes
```

`enter` launches a child shell. Exit it with `exit` or Ctrl-D.

## End-to-end circuit checks

Network Sandbox automatically captures established TCP connections on Linux.
For outbound connections it can repeat a direct TCP reachability check.
Application-level connectivity should use a safe canary command:

```bash
sudo netsandbox circuit add proxy-change codex-api -- \
  curl --fail --silent --show-error \
  --max-time 5 https://example.invalid/health
```

The command is executed without a shell, from inside the experimental
filesystem and network view. This lets an agent register a non-mutating
self-test that uses its real proxy settings, credentials, DNS, and routes.

Then validate:

```bash
sudo netsandbox check proxy-change
sudo netsandbox connections proxy-change
```

Required circuits have these safety states:

- `Preserved`: the experimental view passed its check.
- `Lost`: a previously available capability failed.
- `Unverifiable`: no safe replay method exists.
- `Ignored`: explicitly non-blocking.

`Lost`, stale, and `Unverifiable` required circuits block apply by default.
Inbound circuits are deliberately `Unverifiable` until a paired external probe
is available; the server cannot safely impersonate the original client.

Manage exceptional circuits with:

```bash
netsandbox circuit add-tcp ENVIRONMENT NAME 203.0.113.10:443
netsandbox circuit add-tcp ENVIRONMENT NAME 203.0.113.10:443 --interface utun6
netsandbox circuit ignore ENVIRONMENT CIRCUIT_ID
netsandbox circuit require ENVIRONMENT CIRCUIT_ID
netsandbox circuit remove ENVIRONMENT CIRCUIT_ID
```

## Filesystem behavior

On native Linux, the host is the read-only lower layer and the sandbox stores only
paths that were added, changed, or deleted:

```text
sandbox view = host lower layer + sandbox upper layer
```

The native macOS runner materializes only explicitly staged paths into an
ephemeral workspace, constrains child writes to that workspace, and
synchronizes only declared paths into the persistent upper layer. The legacy
Linux-image runner stores Docker's writable layer; the legacy VM runner uses a
prepared guest disk rather than the host root.

Useful inspection and reset commands:

```bash
netsandbox stage proxy-change /etc/example-proxy/config
netsandbox stage proxy-change /etc/example-proxy/config --from /tmp/candidate
netsandbox stage proxy-change /etc/example-proxy/obsolete.conf --delete
netsandbox changes proxy-change
netsandbox diff proxy-change /etc/example-proxy/config
netsandbox reset proxy-change /etc/example-proxy/config
netsandbox reset proxy-change --all --yes
```

On Linux and the native macOS workflow, reset affects the experimental upper
layer. For a managed legacy macOS VM, use `mac vm-reset --yes` to rewind the
guest disk as well.

```bash
netsandbox discard proxy-change --yes
```

## Apply and rollback

`plan` blocks apply when:

- there are no changes;
- a required circuit is lost, unverifiable, or stale;
- a host file changed after Network Sandbox recorded its origin.

Apply creates backups before touching the host and uses atomic replacement for
regular files:

```bash
sudo netsandbox apply proxy-change --yes
netsandbox history
sudo netsandbox rollback apply-YYYYMMDD-HHMMSS-ID --yes
```

Application canaries run again after apply. A failure triggers rollback when
`auto_rollback` is enabled, which is the default.

## Agent integration

An agent should not merely be instructed to remember Network Sandbox. Its
mutation runner should enforce the lifecycle:

1. Create or select an environment.
2. Register the agent's own safe API/heartbeat canary.
3. Execute every mutating command through `netsandbox exec`. On macOS, stage
   every writable path first; use configuration-aware arguments for native
   execution and `mac route-preview` for routes.
4. Run `check` and `plan`.
5. Require explicit authority before `apply`.

Example:

```bash
sudo netsandbox exec proxy-change -- systemctl restart example-proxy
```

Read-only discovery commands may run outside the sandbox. Commands that can
modify files, credentials, packages, services, routing, DNS, firewall rules, or
proxy configuration must run inside it.

## Current safety boundaries

This preview intentionally fails closed, but it is not a universal
production-grade safety boundary:

- A paired external client probe for new inbound SSH/proxy sessions is not yet
  implemented.
- A detached rollback guard watches a heartbeat lease and restores an
  uncommitted real-host transaction if the applying process disappears. A
  production release should replace the short-lived guard with a permanently
  installed, privilege-separated `netsandboxd`.
- Automatic TCP observation proves transport reachability, not application
  authentication. Use an application canary for the real circuit.
- Host conflict origins are captured when a changed path is first inspected.
  A production filesystem observer must capture them at the first copy-up
  event.
- File transactions preserve permission modes, owner/group, and extended
  attributes. Native macOS ACLs, service dependency ordering, IPv6 sandbox
  routing, and special-file application still need additional hardening.
- Native macOS command execution is path-declared rather than a transparent
  cloned root. Hard-coded paths, privileged service delegation, launchd,
  SystemConfiguration, PF, VPN, and Network Extension mutations need typed
  adapters and otherwise remain blocked.
- The Linux backend currently permits one active sandbox at a time.
- The experimental macOS VM backend isolates arbitrary guest commands, but it
  is not a transparent copy of the live host. Promotion remains limited to
  tracked regular files and typed native route candidates.
- The macOS Linux-image backend is lightweight and fully copy-on-write, but its
  apply target is a new Linux image, never the macOS host. Images must contain
  the required POSIX tools and use a libc/architecture compatible with the
  companion helper.
- A paired external client probe and a production-grade signed VM helper/guest
  agent are still future work.

See [architecture.md](architecture.md) for the component and safety
model, and
[macos-isolation-research.md](macos-isolation-research.md) for the
backend analysis and primary sources.
