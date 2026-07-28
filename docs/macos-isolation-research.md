# macOS isolation backend research

_Research date: 2026-07-27. Scope: Apple silicon. Sources are Apple documentation, project-owned documentation/source, and the macOS manuals shipped by Apple._

## 2026-07-28 implementation decision

The product requirement is capability equivalence for supported
connectivity-changing operations, not transparent execution of every macOS
command against a cloned root and network namespace. Under that definition,
the default backend is now a native differential adapter:

- stage every writable file explicitly;
- materialize those paths in an ephemeral candidate workspace;
- rewrite configuration-aware path arguments;
- constrain an unprivileged child so it can write only in that workspace;
- allow real protocol testing and shadow-service listeners;
- synchronize only declared paths to the differential layer; and
- keep route and other kernel changes behind typed preview/apply adapters.

Seatbelt is used here as write-confinement defense, not as a claim of a private
network stack or transparent filesystem. The VM conclusion below still applies
when arbitrary-command transparency is required.

## Recommendation

There is no currently practical backend that satisfies all four strict requirements at once:

1. run arbitrary **macOS** commands;
2. present a writable copy-on-write view of the current host filesystem;
3. provide an independent, realistically mutable network stack; and
4. remain lightweight enough for routine CLI startup while supporting generic transactional promotion back to the host.

The hard boundary is the network stack. A VM supplies independent routes, interfaces, DNS, sockets, and firewall state; filesystem overlays, App Sandbox, PF, and Network Extension do not. Conversely, the available lightweight VM/container systems boot a guest filesystem, not a copy-on-write projection of the live macOS root. Apple’s container runtime and the normal Lima path run Linux, so they cannot faithfully rehearse macOS commands such as `networksetup`, `scutil`, `launchctl`, or macOS binaries.

The recommended product shape is therefore:

- Keep the existing native macOS backend as the default, host-faithful, apply-capable path. Continue limiting it to typed route previews/tests and explicitly staged files.
- Add an opt-in `vm-linux` runner using Apple `container` when Linux command semantics are acceptable. Describe it as isolated Linux execution, not as general macOS `exec`.
- Treat a `vm-macos` runner built on Virtualization.framework as a research/CI backend. It is the only credible route to arbitrary macOS commands plus independent network state, but it uses a prepared guest image rather than the live host root and is not lightweight.
- Do not describe macFUSE, Seatbelt, PF, or Network Extension as a private
  macOS root and network namespace. Seatbelt can enforce the native
  path-declared adapter, while PF and Network Extension remain typed
  host-integration options rather than generic isolation.
- Keep `apply` typed and allowlisted. An arbitrary guest filesystem or network diff is not generically meaningful on the host; “transactional apply” must translate supported intents into the project’s existing validated operations.

If “arbitrary macOS commands against a COW copy of the live host” is non-negotiable, the honest result is **no shippable backend today**. A macOS VM is the closest architecture, but it still fails the live-root and lightweight requirements.

## Capability matrix

| Candidate | Writable filesystem isolation | Independent network state | Runs macOS commands | CLI weight | Result |
|---|---|---|---|---|---|
| Virtualization.framework macOS VM | Yes, against a prepared guest disk; APFS-cloned disk images can make per-run copies cheap | Yes; NAT, bridged, or custom virtual NIC topology | Yes | High: macOS image, boot, memory, guest agent | Closest strict boundary, but not a live-host COW view and not lightweight |
| Apple `container` / `containerization` | Yes; OCI layers are unpacked to an ext4 image and the runtime clones the root filesystem COW | Yes; each Linux container is a VM with its own IP; internal networks are available | No, Linux only | Low for a VM; advertised sub-second startup | Best near-term isolated runner if Linux semantics are acceptable |
| Lima | Yes, for a Linux/macOS guest disk; host mounts are separate and should be read-only | Yes, via user-mode, VZ NAT, or vmnet networking | Linux normally; macOS guest support is experimental | Medium | Useful prototype/fallback, not the core backend |
| Tart | Yes, for a VM image | Yes, at VM boundary | Yes | High; the quick-start macOS image is about 25 GB | Ready-made macOS-VM experiment, not a lightweight host projection |
| APFS clone/snapshot | Fast block/file COW primitive; snapshots themselves are read-only | No | Only if combined with another execution boundary | Low | Storage accelerator only |
| macFUSE union/overlay | Can expose a writable union at a mount point | No | A process still sees the real `/` unless separately re-rooted | Medium; kernel backend has deployment costs | Not an isolation backend |
| `sandbox-exec` / App Sandbox | Denies selected writes or network access; does not redirect writes into a COW layer | No private routes/interfaces/DNS/firewall | Yes | Very low | Policy restriction only; direct `sandbox-exec` is deprecated |
| Network Extension or PF | None | Can filter/proxy traffic, but operates on host traffic/state rather than cloning the stack | Yes | Medium to high operational cost | Useful enforcement/transport components, not a namespace |

## Candidate findings

### 1. Virtualization.framework

Virtualization.framework is the only Apple-supported primitive in this set that gives a guest its own operating system and network stack. Apple documents macOS guests on Apple silicon, installed from an IPSW into a VM bundle containing a disk image and machine-specific auxiliary state. This is a **guest baseline**, not a projection of the running host. ([Run macOS in a VM](https://developer.apple.com/documentation/virtualization/running-macos-in-a-virtual-machine-on-apple-silicon), [install macOS in a VM](https://developer.apple.com/documentation/virtualization/installing-macos-on-a-virtual-machine))

`VZDiskImageStorageDeviceAttachment` supports raw and ASIF disk images. A per-run copy of a prepared image can be cheap on APFS because Apple documents that ordinary copies on APFS are automatically cloned and become copy-on-write. Shared directories are explicit host exposures and can be read-only; they are not a root overlay. ([disk-image attachment](https://developer.apple.com/documentation/virtualization/vzdiskimagestoragedeviceattachment), [APFS cloning](https://developer.apple.com/documentation/foundation/about-apple-file-system), [shared directories](https://developer.apple.com/documentation/virtualization/shared-directories))

For networking, VZ offers NAT, bridged, and custom vmnet-backed attachments. NAT allows the guest to reach outside networks without the bridged-network entitlement; bridged networking requires `com.apple.vm.networking`. A no-NIC or private custom topology can be used when external side effects must be prevented. ([NAT attachment](https://developer.apple.com/documentation/virtualization/vznatnetworkdeviceattachment), [bridged attachment](https://developer.apple.com/documentation/virtualization/vzbridgednetworkdeviceattachment), [vmnet attachment](https://developer.apple.com/documentation/virtualization/vzvmnetnetworkdeviceattachment/))

The implementation needs a small signed Swift/Objective-C helper with Apple’s virtualization entitlement, plus a guest agent reached over virtio sockets for command execution and artifact collection. ([virtualization entitlement](https://developer.apple.com/documentation/virtualization/adding-the-virtualization-entitlement-to-your-project), [virtio socket device](https://developer.apple.com/documentation/virtualization/vzvirtiosocketdevice))

macOS 27’s beta DiskImageKit adds true stacked raw/ASIF images with an upper overlay that receives writes. That is a promising VM-disk primitive, but it is beta-only and still overlays a VM image rather than the live host root. It should be a feature-gated optimization, not a baseline dependency. ([DiskImageKit](https://developer.apple.com/documentation/diskimagekit), [WWDC26: Manage disk images with DiskImageKit](https://developer.apple.com/videos/play/wwdc2026/224/))

### 2. Apple `container` and `containerization`

Apple’s `container` CLI runs each Linux container inside its own lightweight VM on Apple silicon and macOS 26. The underlying `containerization` library uses Virtualization.framework, gives containers dedicated IP addresses, and advertises sub-second startup. Its root filesystem is an OCI Linux filesystem unpacked into ext4, not macOS or the host root. ([`container` repository](https://github.com/apple/container), [`containerization` repository](https://github.com/apple/containerization), [technical overview](https://github.com/apple/container/blob/main/docs/technical-overview.md), [WWDC25 introduction](https://developer.apple.com/videos/play/wwdc2025/346/))

The runtime source explicitly implements root-filesystem cloning as a copy-on-write `clonefile` operation. The CLI also supports read-only mounts, a read-only root, command execution, export, and internal networks. These make it the strongest lightweight option for untrusted or experimental **Linux** commands. ([filesystem clone source](https://github.com/apple/container/blob/main/Sources/ContainerResource/Container/Filesystem.swift), [command reference](https://github.com/apple/container/blob/main/docs/command-reference.md), [snapshot store](https://github.com/apple/container/blob/main/Sources/Services/ContainerImagesService/Server/SnapshotStore.swift))

Important gaps:

- It cannot execute macOS binaries or reproduce macOS networking tools.
- Writable bind mounts would violate the host-preservation invariant; host inputs must be copied in or mounted read-only.
- Starting the runtime creates helper processes, runtime metadata, and virtual-network infrastructure on the host. This is compatible only with an operational definition of “host unchanged” that permits private runtime state while forbidding target filesystem/network mutations.
- Export is a guest filesystem artifact, not a ready-to-apply Network Sandbox upper layer.

### 3. APFS snapshots and clones

APFS clones are an excellent optimization for prepared VM disks and copied input trees: they initially share storage and allocate changed blocks on write. APFS snapshots are read-only point-in-time copies of a volume. Neither primitive changes process path resolution or supplies a private network stack. Modern macOS also separates the writable data volume from a sealed system snapshot, making a faithful alternate-root projection substantially more complex than cloning a directory tree. ([Apple File System overview](https://developer.apple.com/documentation/foundation/about-apple-file-system), [APFS security and sealed system volumes](https://support.apple.com/guide/security/role-of-apple-file-system-seca6147599e/web), [APFS snapshots](https://support.apple.com/guide/disk-utility/view-apfs-snapshots-dskuf82354dc/mac))

Attaching the host’s physical disk writable to a VM is not a shortcut: Apple explicitly warns that a guest with raw block access can irrecoverably destroy the disk. ([block-device attachment](https://developer.apple.com/documentation/virtualization/vzdiskblockdevicestoragedeviceattachment))

### 4. macFUSE overlays

macFUSE is a bridge for implementing user-space filesystems; it does not itself provide an overlay filesystem. A separate project such as `unionfs-fuse` can expose COW union semantics at a mount point, but an arbitrary macOS process still resolves `/` against the host unless another mechanism re-roots it, and macOS has no Linux-style mount namespace. It also supplies no private network state. ([macFUSE](https://macfuse.github.io/), [macFUSE setup and deployment](https://github.com/macfuse/macfuse/wiki/Getting-Started), [`unionfs-fuse`](https://github.com/rpodgorny/unionfs-fuse))

The newer FSKit backend reduces kernel-extension friction on macOS 15.4+, while macFUSE’s full kernel backend on Apple silicon can require reduced security, user approval, and restart. That deployment cost is disproportionate for a component that still solves only part of the problem.

### 5. `sandbox-exec` and App Sandbox

Apple’s installed `sandbox-exec(1)` manual marks the tool deprecated. Its profiles restrict operations inherited by child processes, but allowed writes still reach the host and denied writes fail; there is no redirection into a COW upper layer. App Sandbox likewise provides entitlement-based access control and an application container, not an alternate host root or independent networking configuration. ([App Sandbox configuration](https://developer.apple.com/documentation/xcode/configuring-the-macos-app-sandbox), [sandboxed file access](https://developer.apple.com/documentation/security/accessing-files-from-the-macos-app-sandbox))

This is useful as the write-confinement layer for a path-declared native
adapter, but it cannot supply arbitrary-command root or network-namespace
semantics.

### 6. Network Extension and PF

Network Extension can proxy flows or implement a VPN-style packet tunnel, but it does not clone the host routing table, interfaces, DNS configuration, or PF state for one arbitrary child process. Apple documents transparent proxies as flow interception and notes that some traffic remains direct; packet-tunnel providers are intended for VPN use rather than as a general process namespace. Deployment also requires entitlements and a containing signed/notarized application or system extension. ([transparent proxy provider](https://developer.apple.com/documentation/networkextension/netransparentproxyprovider), [packet-tunnel expected uses](https://developer.apple.com/documentation/technotes/tn3120-expected-use-cases-for-network-extension-packet-tunnel-providers), [Network Extension entitlement](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.developer.networking.networkextension), [system extensions](https://developer.apple.com/documentation/systemextensions))

Apple’s installed `pfctl(8)` and `pf.conf(5)` manuals describe PF as host-global filtering/NAT state controlled through `/dev/pf`, with rules and anchors attached to host interfaces. A dedicated anchor can reduce collision risk, but loading it is itself a privileged host network mutation. PF is therefore suitable for an explicit apply operation or narrow safety guard, not rehearsal isolation.

### 7. Lima and Tart

Lima normally launches Linux VMs and defaults to Virtualization.framework on supported macOS versions. It offers read-only host mounts and user-mode, VZ NAT, or privileged vmnet options. These are useful for a prototype and for older hosts that cannot run Apple `container`, but they retain the Linux/host-projection mismatch. Lima’s macOS guest and snapshot features are explicitly experimental. ([Lima](https://github.com/lima-vm/lima), [VZ driver](https://lima-vm.io/docs/config/vmtype/vz/), [mounts](https://lima-vm.io/docs/config/mount/), [user-mode network](https://lima-vm.io/docs/config/network/user/), [macOS guests](https://lima-vm.io/docs/usage/guests/macos/), [experimental features](https://lima-vm.io/docs/releases/experimental/))

Tart is a mature Apple-silicon VZ wrapper aimed at macOS/Linux CI VMs. It is the quickest way to prototype a macOS guest runner, but its own quick start pulls an approximately 25 GB macOS image and uses VM/SSH workflows. That is strong isolation, not a lightweight per-command host sandbox. ([Tart](https://github.com/cirruslabs/tart), [Tart quick start](https://tart.run/quick-start/))

## Proposed implementation path

### Phase 0 — make capabilities explicit

Add backend capability descriptors rather than treating `exec` as universally available:

- `native-macos`: typed route preview/test, explicit staged files, transactional apply.
- `vm-linux`: arbitrary Linux execution, isolated guest filesystem/network, no claim of host fidelity.
- `vm-macos`: arbitrary macOS guest execution, isolated guest filesystem/network, prepared-image fidelity only.

Reject a scenario before launch if it requires unavailable semantics. In particular, never silently run a macOS rehearsal in a Linux guest.

### Phase 1 — experimental `vm-linux`

Dependencies: Apple silicon, macOS 26+, Apple `container`, an OCI Linux image, and a version-pinned guest toolset.

1. Start a fresh container VM from a pinned image.
2. Copy scenario inputs into the guest or mount them read-only; prohibit writable host binds.
3. Use an internal network or no external attachment by default.
4. Execute through `container run`/`exec`.
5. Export only declared result paths and a machine-readable typed-intent journal.
6. Destroy the ephemeral instance after collection.
7. Feed supported intents through the existing preview, validation, transaction, and rollback engine.

This phase delivers useful isolated command execution quickly, but must be opt-in because it does not model macOS.

### Phase 2 — macOS VM proof of concept

Dependencies: Apple silicon, compatible macOS/Xcode SDK, a signed VZ helper with the virtualization entitlement, a prepared/versioned macOS IPSW image, APFS storage, a guest agent, and virtio-socket IPC.

1. Maintain a sealed, patched VM template with the guest agent installed.
2. APFS-clone its disk and auxiliary VM bundle into private per-run storage.
3. Boot with NAT for ordinary guest isolation, or no NIC/private topology for strict offline rehearsal.
4. Send command, environment, and timeout over virtio sockets; never expose the host root writable.
5. Collect declared files plus normalized snapshots of supported network surfaces.
6. Translate only allowlisted differences—initially files and routes—into existing typed candidates.
7. Delete the ephemeral clone after the transaction is committed, rolled back, or abandoned.
8. On macOS 27+, evaluate DiskImageKit stacked ASIF overlays behind a feature flag.

Prototype this first with Tart to measure boot latency, disk growth, guest-agent reliability, and diff quality. Replace Tart with a small VZ helper only if the measurements justify owning that code.

### Phase 3 — promotion gates

Do not call either VM backend capability-equivalent until it passes:

- zero writes to user-selected host paths before `apply`;
- zero host route, DNS, interface, PF, or service changes during rehearsal;
- no writable shared directories or raw host block devices;
- deterministic cleanup after crash or forced termination;
- an allowlisted, reviewable diff with symlink/path-escape and metadata validation;
- atomic apply/rollback through the current transaction journal; and
- explicit documentation of guest-vs-host semantic differences.

The central design rule should remain: **isolation may be broad, but promotion must be narrow and typed**. That preserves the project’s safety invariant even when arbitrary commands run inside a VM.
