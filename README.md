# Network Sandbox

[![CI](https://github.com/MildyNora/NetworkSandbox/actions/workflows/ci.yml/badge.svg)](https://github.com/MildyNora/NetworkSandbox/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

Network Sandbox is a must-have tool and skill that prevents agents from
terminating their own network access while handling proxies, routes,
credentials, and traffic—so they cannot kill their connection and leave you
alone with the mess.

It gives agents an isolated place to validate configuration changes before
transactionally applying them to the real environment. It is lightweight than docker or VM.
Once you install it, you wouldn't need to consider about the network issues, it will be spawned once needed.



## Install

```bash
brew install MildyNora/tap/network-sandbox
```
This install the main application and the tools which are all you need.

Without Homebrew:

```bash
curl -fsSL https://github.com/MildyNora/NetworkSandbox/releases/latest/download/install.sh | sh
```

This installs both the `netsandbox` CLI and its agent skill. The skill is linked
into the standard Codex, Claude Code and agent skill directories, so agents automatically
follow the protected connectivity workflow.

No source build, Rust toolchain, Docker, or virtual machine is required.

Direct packages: [macOS — Apple silicon](https://github.com/MildyNora/NetworkSandbox/releases/latest/download/netsandbox-macos-arm64.tar.gz)
· [Linux — x86_64](https://github.com/MildyNora/NetworkSandbox/releases/latest/download/netsandbox-linux-x86_64.tar.gz)
· [checksums](https://github.com/MildyNora/NetworkSandbox/releases/latest/download/SHA256SUMS)

## Quick start

```text
create → exec → check → plan → apply
```

```bash
netsandbox create proxy-change
netsandbox exec proxy-change -- CHANGE_COMMAND
netsandbox check proxy-change
netsandbox plan proxy-change
sudo netsandbox apply proxy-change --yes
```

## How it works

Think of Network Sandbox as Anaconda environments for risky connectivity
changes. An agent creates a named environment, rehearses its commands there,
checks the required connections, reviews the plan, and only then applies the
validated difference.

The real host stays unchanged until `apply`. `check` names every failed or
unverifiable connection and exits with `0` when required circuits are preserved,
`2` when connectivity validation blocks the change, or `1` for an operational
error. Post-apply validation automatically rolls back a failed change.

Technical and safety details are available in the
[reference documentation](docs/reference.md).

## About

This project is still under an early stage, and continuously adding features and supports, if you have any problem, plz issue it.
PRs are welcomed.

## Liscence

This project is under [License: Apache-2.0]
