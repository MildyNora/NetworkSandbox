---
name: network-sandbox
description: Safely diagnose, test, rehearse, and plan connectivity-affecting changes with the netsandbox Rust CLI. Use whenever Codex handles any general connectivity task, including SSH or remote access, proxy/VPN/TUN/PAC behavior, DNS, routes, interfaces, firewall rules, ports, TLS/HTTP/SOCKS, connection credentials, connection-critical configuration files, service restarts, agent/API reachability, or any command that could disconnect the active session. Also use before an agent mutates Linux or macOS network state or filesystem state that controls connectivity.
---

# Network Sandbox

Use Network Sandbox as the execution boundary for connectivity work. Introduce it on first use:

> Network Sandbox rehearses connection-sensitive changes, compares required circuits, and keeps the host unchanged until an explicit apply.

Treat the active agent connection as a required control circuit.

## Start with preflight

Run the dependency-free read-only preflight:

```bash
netsandbox --version
netsandbox doctor
```

If the shell cannot find `netsandbox`, ask the user to install the package. Do not replace it with
direct network mutations. If `doctor` reports an unsupported backend, continue read-only diagnosis
and stop before mutation.

`netsandbox` owns this workflow and does not require the legacy NCS helper, `ncsd`, or
`/var/run/ncsd.sock`. Do not invoke or wait for NCS unless the user explicitly requests the
separate NCS ProxyProfile workflow.

Read [references/commands.md](references/commands.md) when constructing commands. Read
[references/use-cases.md](references/use-cases.md) for SSH, proxy, routing, DNS, firewall, or agent
heartbeat work.

## Enforce these boundaries

1. Run read-only discovery before creating a candidate.
2. Never place passwords, keys, tokens, proxy URLs containing credentials, or subscription secrets
   in command arguments, environment metadata, or chat output.
3. Record required connectivity before mutation. Add an application canary when raw TCP does not
   prove the real capability.
4. Treat `Lost`, stale, and required `Unverifiable` circuits as blockers.
5. Never interpret a successful TCP connection as proof that SSH, TLS, authentication, proxying,
   or an API request works.
6. Never run `apply` unless the user explicitly requests a real host change in the current turn.
   Diagnosis, debugging, preview, testing, and planning do not authorize apply.
7. Never use `--force`, weaken a failure policy, ignore a circuit, or disable rollback merely to
   obtain a passing plan.
8. If Network Sandbox cannot represent the candidate, state the limitation and stop before live
   mutation. Do not improvise route, DNS, firewall, VPN, proxy, or credential changes.

## Choose the platform workflow

### Linux

Use the full lifecycle:

```text
create → register circuits → exec/enter → check → diff → plan → apply or discard
```

Create one named environment per task. Route every mutating command through `netsandbox exec`, or
run it in an entered shell. Keep read-only inspection outside when useful for comparison.

After mutation, run connectivity checks and inspect filesystem differences. Require a clean plan.
Prefer `apply --dry-run`; apply only with explicit user authorization. Retain the transaction ID
for rollback.

### macOS

Use the dependency-free native differential runtime and typed route transaction lifecycle by
default:

```text
create → register circuits → stage → exec/enter → check → diff/plan → apply or discard
```

`route-preview` and `mac test` do not change the host routing table. Use `stage` for explicit
copy-on-write file replacements or deletions. Native `exec` maps every staged absolute path
argument (including `--option=/path`) to an ephemeral candidate workspace. The child is
unprivileged, may read the host and use inbound/outbound networking, and may write only inside
that workspace. Only previously staged paths are synchronized back to the differential layer;
undeclared output blocks the command. `NETSANDBOX_CANDIDATE_ROOT` identifies the candidate root
inside `enter`.

Use configuration-aware flags so a shadow process consumes the mapped candidate—for example,
`daemon --config /absolute/staged/path --foreground --listen TEMPORARY_PORT`. Keep it running
under `netsandbox exec` while a separate `netsandbox check` or external paired probe exercises
the temporary endpoint. A program that reads a hard-coded absolute path, delegates mutation to a
privileged macOS service, or requires private kernel route/firewall state is not representable by
native `exec`; use a typed adapter or stop.

After all representable required circuits pass, use `apply --dry-run`, then use real-host
`apply --yes` only with explicit authorization. Keep file and route changes in separate
environments. The apply transaction saves the old state, verifies live circuits, and
automatically rolls back on failure. A detached rollback lease also restores an uncommitted
transaction if the applying process disappears.

Associate an application-level canary with every route candidate using `mac route-canary`; TCP
alone must not authorize apply. If the staged host route differs from the live route, macOS
cannot select the exact candidate route per socket. `mac test` must mark the associated checks
`Unverifiable`, revalidate ordinary control circuits, and keep the host unchanged. A nonzero
result is expected in this case.

Resolve that specific cycle only with the built-in guarded route transaction:

```bash
netsandbox apply NAME --dry-run --trial
sudo netsandbox apply NAME --trial --yes
```

The real command still requires explicit user authorization in the current turn. Guarded trial
must contain typed route changes only, retain automatic rollback, and have fresh preserved
unrelated control circuits. It arms the detached rollback lease before route mutation, installs
the candidate, reruns every route-associated TCP and application canary plus all control
circuits, and commits only if every required result is `Preserved`. Otherwise it restores the
original route. This workflow does not use `ncsd` or any external helper.

Do not ask the user to race the 60-second freshness window after authorization. Real `apply`
automatically refreshes every required non-deferred circuit after administrator elevation and
then constructs the final plan. A prior dry-run remains useful for review, but its timestamps do
not need to survive an interactive password prompt.

The native macOS runtime allows the literal `/dev/null` character device required by SSH and
common Unix programs. Use the real application canary when it now runs successfully. Do not
replace it with a narrower configuration-only check merely because an older Network Sandbox
version denied `/dev/null`; update the binary and retry. Continue to require an external paired
probe when the capability itself is inbound from another machine.

The Docker Linux-image and Lima macOS-VM integrations are legacy optional guest backends. Do not
install or select them merely to obtain routine macOS rehearsal. Use them only when the user
explicitly chooses a guest artifact and accepts that it does not model the live Mac.

For an explicitly selected Linux image:

```text
create → register circuits → linux-create → linux-track → exec/enter → check → linux-diff → linux-commit or discard
```

Run `mac linux-track` for every file before it changes. `exec`, `enter`, and `check` run in the
managed Docker container with no writable host mount. Require a fresh, preserved application
canary and inspect `mac linux-diff`; any untracked container-layer change blocks commit. Use
`mac linux-reset --yes` to recreate the environment from its original image.

`mac linux-commit` creates a new immutable image; it never applies Linux files, routes, DNS, or
firewall state to the Mac. It preserves the source image and records the exact output image ID.
Use `mac linux-rollback --yes` to remove only that recorded output. Do not substitute host
`plan/apply` for this workflow.

For an explicitly selected prepared macOS guest:

```text
create → register circuits → vm-clone → vm-track → exec/enter → check → diff → plan → apply or discard
```

Run `mac vm-track` for every regular guest file that may be promoted before executing mutations.
`exec`, `enter`, and `check` then run in the cloned guest. Tracked files are exported to the
differential layer after commands; `mac vm-sync` exports them on demand. `discard` removes a clone
created by Network Sandbox, but never deletes a user-owned VM connected with `vm-attach`. Use
`mac vm-reset --yes` to recreate a managed clone from its prepared base; never use it on an
attached instance.

Do not describe either guest backend as a clone of the live Mac. Each has an independent
filesystem and network stack. Only tracked regular files from the macOS VM are exported; guest
route, DNS, firewall, VPN, Network Extension, service, directory, symlink, ownership, ACL, and
extended-attribute changes are not automatically promotable. Use native typed route candidates
for host routes. If guest fidelity or promotion support is insufficient, report the boundary and
keep apply blocked.

## Validate the real capability

Use automatic TCP discovery as a starting point, not the final proof.

- For an outbound service, add a safe application-level canary that exercises the actual proxy,
  DNS, TLS, authentication, and configuration path.
- For inbound SSH or proxy access, require a new connection from the original client or a paired
  external probe. Mark it `Unverifiable` when no external probe exists.
- For an agent/API connection, register a non-mutating heartbeat or health command supplied by the
  integration. Do not invent an endpoint that might create side effects.
- For route candidates on macOS, preflight what is representable, add an application canary, and
  use guarded trial only when exact route checks must be deferred until after temporary install.

## Report the outcome

State:

- the environment and candidate tested;
- which circuits were `Preserved`, `Lost`, or `Unverifiable`;
- whether the host remained unchanged;
- whether apply is blocked or authorized;
- the apply transaction ID and rollback status, if an authorized apply occurred.

Do not say a repair succeeded unless the end-to-end circuit passes after the change.
