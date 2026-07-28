# Connectivity Use Cases

## Remote SSH or credential changes

1. Treat the current SSH session as protected.
2. Create an environment before editing `sshd_config`, authorized keys, certificates,
   credentials, PAM, firewall rules, or routes. On macOS, use `stage` for each changed file and
   run configuration-aware validators or shadow services through native `exec`.
3. Require a new inbound SSH attempt from the original client. The existing established session
   is not proof that a new login works.
4. Run `check`, inspect `diff`, and require a clean plan.
5. Never apply while inbound SSH is `Unverifiable`.

On macOS, retry the full SSH canary with the current binary if an older run failed only because
the native sandbox denied `/dev/null`. Use configuration-only checks as supplemental evidence,
not as a replacement for end-to-end SSH.

## Proxy, VPN, TUN, or PAC debugging

1. Capture the current agent/API circuit and ordinary HTTPS control.
2. Add a safe application canary that uses the same proxy semantics as the affected application.
3. Test candidate configuration inside native Linux isolation or the native macOS differential
   runner. On macOS, pass staged absolute configuration paths to a foreground shadow service and
   use typed interface-bound route previews.
4. Compare direct TCP, TLS/application behavior, DNS behavior, and the end-to-end agent heartbeat.
5. Do not equate a listening local proxy port with a working upstream.

## Route or interface changes

### Linux

Run route mutations only inside the Linux environment. Validate every previously required circuit
from the experimental network view.

### macOS

Use:

```bash
netsandbox mac route-show DESTINATION
netsandbox mac route-preview NAME DESTINATION --interface INTERFACE [--gateway GATEWAY_IP] --port PORT
netsandbox mac route-canary NAME CANDIDATE_ID LABEL -- PROGRAM ARGUMENT...
netsandbox mac test NAME
```

This preflights connection-scoped interface selection without changing the routing table. It
cannot select an exact staged host route or gateway per socket, so checks associated with a route
that differs from the live route are deferred as `Unverifiable`. A preserved TCP port proves
transport only, so every candidate also needs a required application canary. For an exact route
change, run `apply NAME --dry-run --trial`, and use
`sudo netsandbox apply NAME --trial --yes` only after explicit authorization. Guarded trial
installs only the typed route under rollback protection and commits only if every required
circuit passes. If the desired candidate includes VPN or Network Extension state that cannot be
represented, report it as unsupported instead of applying a live workaround.

Administrator prompting may take longer than the normal freshness window. Do not refresh checks
manually after the prompt: real apply performs a final non-deferred refresh after elevation,
builds the plan, and proceeds directly to the guarded transaction.

## DNS or firewall work

1. Capture DNS, API, SSH, package repository, and control-plane requirements.
2. Run file and command mutations inside Linux isolation. On macOS, stage every writable path and
   run a configuration-aware command through native `exec`.
3. Validate hostname resolution separately from literal-IP reachability.
4. Test both inbound and outbound firewall direction.
5. Block apply when an external inbound probe is unavailable.

## Agent-driven system administration

Enforce Network Sandbox in the agent's command runner rather than relying on memory:

1. Allow read-only discovery outside.
2. Create or select one environment for the task.
3. Register the agent heartbeat and task-specific circuits.
4. Route every mutating command through `netsandbox exec`. On macOS, stage every writable path
   first and use typed route candidates for kernel network state; never mutate the host directly.
5. Run `check`, `diff`, and `plan`.
6. Request explicit user authorization before `apply`.

If the agent disconnects or the terminal closes, do not assume success. Rely on the detached
rollback lease and transaction status.

## Service restarts and arbitrary configuration files

Network Sandbox is path-agnostic: the changed file may belong to SSH, a proxy dashboard, a VPN,
DNS, a firewall, or a custom daemon. Validate the service behavior that consumes the file.

The native macOS runtime can launch a configuration-aware service in the foreground against a
staged file and temporary port. It does not virtualize launchd or an arbitrary privileged system
service. If the service cannot run as an unprivileged shadow instance, or its effects cannot be
represented as staged files or a typed candidate, state that limitation and keep apply blocked.
