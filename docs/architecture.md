# Architecture

## Safety invariant

Until an explicit apply transaction begins, a command inside an environment
must not mutate the host filesystem or host network configuration.

The runtime therefore has two sides:

```text
Host supervisor
  ├── environment metadata
  ├── original connection baseline
  ├── apply/rollback journal
  └── isolated runner
        ├── mount namespace
        ├── OverlayFS upper layer
        ├── PID/UTS/IPC namespaces
        └── network namespace + veth/NAT
```

The supervisor remains outside the experimental namespaces. Probe results
cross the boundary through a dedicated control directory rather than by
promoting the sandbox's state files.

## macOS backends

macOS has no public equivalent of Linux network namespaces and OverlayFS.
Network Sandbox does not claim otherwise.

### Native differential command runtime

The default macOS runtime reaches the same rehearsal/apply safety outcome with
a different, narrower execution contract:

```text
explicit staged paths
  → ephemeral candidate workspace
  → rewrite exact absolute path arguments to candidate paths
  → unprivileged Seatbelt-constrained child
  → host reads plus inbound/outbound test networking
  → reject undeclared candidate output
  → synchronize declared paths to the persistent upper layer
```

The runtime uses only the Rust executable and macOS's installed Seatbelt
facility. It does not require Docker, Lima, a VM image, or a server. The child
may not write outside the ephemeral workspace. It also runs without root so
kernel or privileged service changes cannot be treated as rehearsed file
changes.

Candidate mappings apply to exact absolute arguments and
`--option=/absolute/path` forms. Application canaries inherit the same mapping.
Configuration-aware daemons can therefore run as shadow instances against a
candidate file and temporary port while another process exercises the real
protocol.

This is an adapter boundary, not a transparent cloned root. Hard-coded
configuration lookups, launchd/SystemConfiguration mutations, and private
route, PF, VPN, or Network Extension state require typed native adapters.
Unsupported commands fail closed.

### Legacy Linux-image backend

For Linux workloads on a Mac, Network Sandbox creates a Docker container from
an immutable source image without host bind mounts. Docker's writable layer is
the filesystem difference and the container's bridge namespace is the network
boundary:

```text
immutable Linux source image
  → managed persistent COW container
  → tracked pristine-file baselines
  → arbitrary guest command and circuit validation
  → classify the complete Docker layer diff
  → reject every untracked change
  → commit to a new immutable image
  → remove that exact image ID on rollback
```

The guest helper is copied into a temporary control directory for each command
and removed afterward. Internal control changes are excluded from the
candidate; every other changed path must fall under the explicit tracked-file
allowlist. Commit restores the source image's entrypoint and command. The
source image and Mac host remain unchanged.

This backend intentionally cannot promote Linux paths or network state onto
macOS. Its apply artifact is the validated output image.

### Native typed backend

The default macOS backend combines explicit copy-on-write file staging with
typed route candidates:

```text
 current route observation
  + staged destination/interface/optional-gateway/ports
  + associated application canary
  → interface-bound socket preflight where representable
  → exact route-dependent checks marked Unverifiable
  → explicit guarded-trial plan
  → journal original route
  → arm detached rollback lease
  → install candidate route
  → live route/circuit validation
  → commit or automatic rollback
```

Candidate connections use Apple’s per-socket interface binding options before
`connect(2)` when that models the request. A different host route or gateway
cannot be selected per socket, so its checks are deferred rather than
misreported as failures against the candidate. Normal apply remains blocked.
Explicit `apply --trial` accepts route-only plans, blocks failed or stale
unrelated control circuits, starts rollback protection, installs the route,
then requires every route and application check to pass before commit. A real
apply refreshes required pre-change controls after privilege acquisition and
only then builds the final plan, so interactive authorization time cannot age
otherwise valid evidence out of the 60-second window. Apply uses the native
`route` utility with root privileges, rejects route conflicts, and records
enough state to restore the original host route. Explicitly staged files use
the same backup-backed apply engine as Linux in a separate transaction.

The native Seatbelt profile is read-oriented outside the ephemeral workspace.
It grants only `file-write-data` to the literal `/dev/null` path so standard
Unix clients such as SSH can initialize normally without opening a general
writable-device escape.

### Legacy experimental macOS VM runner

When an environment is connected to a prepared Lima 2.1+ macOS guest,
`enter`, `exec`, and `check` execute across Lima's SSH channel inside a
Virtualization.framework VM:

```text
prepared macOS base instance
  → per-environment Lima clone, with host mounts removed
  → arbitrary guest command
  → guest-local filesystem and network effects
  → guest connectivity probe result
  → export declared regular files
  → existing diff, conflict, apply, and rollback engine
```

The clone gives commands an independent guest filesystem and network stack.
It is not a projection of the running host root. A scenario is meaningful only
when the prepared VM represents the relevant host inputs and contains the
required software and credentials.

Network Sandbox does not infer an arbitrary guest network or service-state
diff. Promotion stays narrow: declared regular files enter the existing upper
layer, while host routes must use typed native route candidates. Managed clones
are deleted on discard/remove and can be recreated from their recorded base
with `mac vm-reset`; attached user-owned instances are never deleted or reset.

The Lima integration is a prototype dependency. A production backend should
replace it with a signed Virtualization.framework helper and a narrow guest
agent while retaining the same typed promotion boundary.

## Differential filesystem

For each environment, the store contains:

```text
environments/NAME/
  environment.json
  upper/
  work/
  merged/
  control/
```

At runtime, the persistent upper layer is staged on tmpfs. This prevents the
upper and work directories from overlapping a `/` lower layer. OverlayFS is
mounted with the selected host base as its lower directory. When the child
command exits, the staged upper layer is copied back with metadata and
whiteouts preserved.

The apply engine scans the upper layer, records content plus metadata digests,
rejects path traversal, checks for host conflicts, creates per-path backups,
and applies regular files with same-directory atomic renames. Permission bits,
owner/group, and extended attributes are preserved through apply and rollback.
Unsupported special files are rejected.

## Network isolation

The Linux runner creates:

- a named network namespace;
- a veth pair with a private `/30`;
- a default route through the host peer;
- a per-environment nftables table for forwarding and masquerading.

The table and namespace are removed when the runner exits. IPv4 forwarding is
temporarily enabled only when required and restored afterward. A runtime lock
limits the current backend to one active environment so this temporary host state
cannot race another Network Sandbox session.

Commands that change routes, addresses, interfaces, or firewall state see only
the sandbox network namespace.

## Capability validation

The Linux baseline reader observes established TCP sockets in `/proc/net/tcp`
and `/proc/net/tcp6`. It infers inbound connections by correlating local ports
with listening sockets.

Validation supports:

1. Direct TCP connection attempts for automatically observed outbound flows.
2. Explicit command canaries for safe application-level checks.
3. External-probe placeholders for inbound flows.

An inbound placeholder is `Unverifiable`, never successful. A future paired
probe must initiate a new session from the original side of the circuit and
report the result to the host supervisor.

## Apply transaction

Apply follows this order:

```text
fresh validation
  → conflict check
  → backup journal
  → detached rollback lease
  → atomic path and route application
  → post-apply route/circuit validation
  → retain transaction or automatically roll back
```

The rollback transaction is independent of the environment so it remains
available after the environment is removed. For real-host transactions, a
detached guard monitors a heartbeat and rolls back an uncommitted transaction
if the applying process or terminal disappears.

## Production hardening path

The next security milestone is a small privileged `netsandboxd` with:

- a Unix-domain control socket and peer-credential authorization;
- durable transaction and fsync discipline;
- copy-up event observation and immediate origin capture;
- crash recovery for abandoned namespaces and nftables tables;
- a paired, authenticated external probe protocol;
- heartbeat leases that trigger rollback without a live terminal;
- privilege separation between storage, probing, and apply operations.
