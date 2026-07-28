# Network Sandbox

[English](README.md) | [简体中文](README.zh-CN.md)

[![Release](https://img.shields.io/github/v/release/MildyNora/NetworkSandbox?display_name=tag)](https://github.com/MildyNora/NetworkSandbox/releases/latest)
[![CI](https://github.com/MildyNora/NetworkSandbox/actions/workflows/ci.yml/badge.svg)](https://github.com/MildyNora/NetworkSandbox/actions/workflows/ci.yml)
[![Platforms](https://img.shields.io/badge/platforms-macOS%20%7C%20Linux-5865F2.svg)](#install)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

**A lightweight safety backbone for agents making connectivity changes.**

Network Sandbox helps agents avoid cutting off their own access while changing
proxies, routes, credentials, and other network-critical configuration. It gives
them an isolated place to rehearse changes, verify required connections, and
apply only validated differences with rollback protection.

The native CLI and agent skill are installed together. Once installed, the
skill tells compatible agents when and how to use the sandbox, so you do not
have to manage every safety step yourself.

## Install

```bash
brew install MildyNora/tap/network-sandbox
```

Without Homebrew:

```bash
curl -fsSL https://github.com/MildyNora/NetworkSandbox/releases/latest/download/install.sh | sh
```

Both methods install the `netsandbox` CLI and its agent skill. The skill is
linked into the standard Codex and agent skill directories, so compatible
agents automatically follow the protected connectivity workflow.

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

Think of Network Sandbox as an Anaconda-style environment for risky
connectivity changes. An agent creates a named environment, rehearses its
commands there, checks the required connections, reviews the plan, and only
then applies the validated difference.

The real host stays unchanged until `apply`. `check` names every failed or
unverifiable connection and exits with `0` when required circuits are preserved,
`2` when connectivity validation blocks the change, or `1` for an operational
error. Post-apply validation automatically rolls back a failed change.

Technical and safety details are available in the
[reference documentation](docs/reference.md).

## About

Network Sandbox is an early-stage project under active development. Issues,
feedback, and pull requests are welcome.

## License

Licensed under the [Apache License 2.0](LICENSE).
