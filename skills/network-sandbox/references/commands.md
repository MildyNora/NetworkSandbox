# Command Reference

## Preflight

```bash
netsandbox --version
netsandbox doctor
netsandbox --help
```

`doctor` is read-only. Stop before mutation when required runtime checks fail. On macOS,
`native enter/exec`, the route backend, and differential apply are required. Lima and Docker are
reported only as legacy optional guest backends.

## Environment lifecycle

```bash
netsandbox create NAME --description "PURPOSE"
netsandbox list
netsandbox status NAME
netsandbox inspect NAME
```

On Linux and on the default native macOS differential runtime:

```bash
sudo netsandbox enter NAME
sudo netsandbox exec NAME -- COMMAND ARGUMENT...
```

Exit an entered shell with `exit` or Ctrl-D.

On macOS, stage each writable file before `exec`. Exact absolute path arguments and
`--option=/absolute/path` values are mapped to the candidate workspace:

```bash
netsandbox stage NAME /absolute/path/to/config
netsandbox exec NAME -- PROGRAM --config=/absolute/path/to/config
```

The macOS child runs without root privileges and cannot write outside its ephemeral workspace.
Only staged paths are synchronized to the environment. Use
`$NETSANDBOX_CANDIDATE_ROOT/relative/path` in an entered shell. Hard-coded paths, privileged XPC
mutations, service-manager changes, and private kernel network state require typed adapters and
must otherwise fail closed.

## Circuit baseline and validation

```bash
netsandbox baseline show NAME
netsandbox baseline refresh NAME --yes
netsandbox connections NAME
netsandbox check NAME
netsandbox watch NAME
```

Baseline refresh changes the safety reference. Use it only when the user intentionally accepts the
new reference state.

Add a direct TCP canary:

```bash
netsandbox circuit add-tcp NAME LABEL 203.0.113.10:443
```

Bind one probe socket to an interface without changing host routes:

```bash
netsandbox circuit add-tcp NAME LABEL 203.0.113.10:443 --interface utun6
```

Add a non-mutating application canary without a shell:

```bash
netsandbox circuit add NAME LABEL -- PROGRAM ARGUMENT...
```

Only use a command known to be safe and repeatable. Never put secrets in its arguments.

Classify exceptional circuits:

```bash
netsandbox circuit require NAME CIRCUIT_ID
netsandbox circuit ignore NAME CIRCUIT_ID
netsandbox circuit remove NAME CIRCUIT_ID
```

Do not ignore a circuit merely to unblock apply.

## Filesystem review

```bash
netsandbox stage NAME /absolute/host/path
netsandbox stage NAME /absolute/host/path --from /path/to/candidate
netsandbox stage NAME /absolute/host/path --delete
netsandbox changes NAME
netsandbox diff NAME
netsandbox diff NAME /absolute/path
netsandbox reset NAME /absolute/path
netsandbox reset NAME --all --yes
```

`reset` affects only the experimental layer.

On macOS, `stage` is the explicit copy-on-write boundary. With no `--from`, it copies the current
host file into the differential layer and prints the candidate path to edit.

## macOS lightweight Linux-image workflow

Use this for Linux workloads on a Mac:

```bash
netsandbox create NAME
netsandbox mac linux-create NAME SOURCE_IMAGE
netsandbox mac linux-track NAME /absolute/linux/file
netsandbox circuit add NAME LABEL -- PROGRAM ARGUMENT...
netsandbox exec NAME -- COMMAND ARGUMENT...
netsandbox check NAME
netsandbox diff NAME
netsandbox mac linux-diff NAME
netsandbox mac linux-commit NAME OUTPUT_IMAGE --yes
```

The optional legacy Linux-image backend requires a compatible
`netsandbox-linux-guest` beside `netsandbox`. Override discovery only when necessary with
`NETSANDBOX_LINUX_GUEST_BIN` or `--guest-binary PATH`.

Track files before mutation. Tracking an already changed path is rejected. `linux-diff` shows the
complete Docker writable-layer diff and marks every path `tracked` or `UNTRACKED`; any untracked
path blocks commit. Required circuits must also be fresh and `Preserved`.

Discard the writable layer or remove a committed output:

```bash
netsandbox mac linux-reset NAME --yes
netsandbox mac linux-rollback NAME --yes
netsandbox discard NAME --yes
```

`linux-reset` preserves the source image and Mac host. `linux-rollback` verifies the recorded image
ID and removes only that output image. Never run host `plan/apply` for a Linux-image environment;
its promotion target is a new image, not macOS.

## macOS isolated VM workflow

Use this only with a prepared Lima 2.1+ macOS base instance that represents the relevant baseline:

```bash
netsandbox create NAME
netsandbox mac vm-clone NAME PREPARED_BASE_INSTANCE
netsandbox mac vm-track NAME /absolute/guest/file
netsandbox exec NAME -- COMMAND ARGUMENT...
netsandbox check NAME
netsandbox diff NAME
netsandbox mac vm-status NAME
netsandbox mac vm-sync NAME
netsandbox mac vm-reset NAME --yes
```

`vm-track` accepts regular files only for export. It maps guest `/path` to the same relative path
under the environment's host `base_root`; it does not copy the host file into the guest. Track
files before the mutating command, and verify that the prepared base contains the expected original
content. A root-owned guest file may require guest-side privilege preparation before an agent can
modify or export it.

For an advanced, user-owned instance:

```bash
netsandbox mac vm-attach NAME EXISTING_INSTANCE
```

`vm-reset --yes` recreates a managed clone from its recorded base and clears tracked exports. It
refuses attached instances. `discard --yes` deletes clones made by `vm-clone`; it does not delete
attached instances. Guest network, route, DNS, firewall, VPN, service, directory, symlink, ACL,
ownership, or xattr changes are not translated from the VM to host apply. Represent host routes through the
typed route workflow below.

## Plan, apply, and rollback

```bash
sudo netsandbox plan NAME
sudo netsandbox apply NAME --dry-run
sudo netsandbox apply NAME --yes
netsandbox history
sudo netsandbox rollback TRANSACTION_ID --yes
```

Use `apply --yes` only after explicit authorization for a real host change. Report the returned
transaction ID. Inspect connectivity after apply. If post-apply validation fails, preserve the
transaction evidence and report whether automatic rollback completed.

Real apply refreshes all required non-deferred circuits after administrator authorization and
before building its final plan. Do not manually repeat checks just to keep their timestamps alive
while waiting for a password prompt. Dry-run never elevates or refreshes checks.

Discard an unapplied experiment:

```bash
netsandbox discard NAME --yes
```

## macOS route workflow

Inspect the currently selected route:

```bash
netsandbox mac route-show DESTINATION
```

Stage a candidate interface and transport checks:

```bash
netsandbox create NAME
netsandbox mac route-preview NAME DESTINATION \
  --interface INTERFACE \
  [--gateway GATEWAY_IP] \
  --port PORT \
  --port PORT
```

Associate at least one non-mutating, application-level canary with the candidate:

```bash
netsandbox mac route-canary NAME CANDIDATE_ID LABEL -- PROGRAM ARGUMENT...
```

The canary must exercise the real application capability. If it needs candidate-interface
selection before apply, the program itself must support and use that binding.

Run candidate probes, every protected baseline circuit, and registered command canaries; then
inspect the plan:

```bash
netsandbox mac test NAME
netsandbox mac route-list NAME
netsandbox apply NAME --dry-run --trial
```

`mac test` can contact every captured outbound endpoint and execute every registered command
canary that is representable without installing the staged route. When the exact candidate route
differs from the live route, associated checks are `Unverifiable` and deferred to guarded trial.
Do not run it when the task forbids those contacts. `--capture-connections false` is only for
synthetic local demonstrations that will never be applied.

Remove candidate metadata:

```bash
netsandbox mac route-remove NAME CANDIDATE_ID
```

The preview and test commands do not change the host. A route that can be fully preflighted may
use normal apply. An exact route or gateway change requires a clean guarded-trial plan and an
explicitly authorized transaction:

```bash
sudo netsandbox apply NAME --yes
sudo netsandbox apply NAME --trial --yes
sudo netsandbox rollback TRANSACTION_ID --yes
```

Never substitute direct `route` mutations. Guarded trial rejects filesystem changes, stale or
failed unrelated control circuits, and disabled automatic rollback. It journals the original
route, starts a detached rollback lease, applies the candidate, rechecks the selected route and
all required circuits, and commits only after those checks pass.

The native macOS executor permits writes only to the literal `/dev/null` device outside its
candidate workspace. SSH canaries may rely on normal `/dev/null` behavior; arbitrary device
writes remain blocked.

## Result meanings

- `Preserved`: the configured probe succeeded.
- `Lost`: the configured probe failed.
- `Degraded`: connectivity changed materially.
- `Unverifiable`: the tool lacks a safe proof method.
- `Ignored`: explicitly non-blocking.
- `Pending`: not tested or no longer fresh.

Required `Lost`, `Unverifiable`, stale, or `Pending` circuits block a normal plan.
