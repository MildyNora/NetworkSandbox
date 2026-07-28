# Network Sandbox

[![CI](https://github.com/MildyNora/NetworkSandbox/actions/workflows/ci.yml/badge.svg)](https://github.com/MildyNora/NetworkSandbox/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

Network Sandbox is a must-have tool and skill that prevents agents from
terminating their own network access while handling proxies, routes,
credentials, and traffic—so they cannot kill their connection and leave you
alone with the mess.

It gives agents an isolated place to validate configuration changes before
transactionally applying them to the real environment.

## Download

- [macOS — Apple silicon](https://github.com/MildyNora/NetworkSandbox/releases/latest/download/netsandbox-macos-arm64.tar.gz)
- [Linux — x86_64](https://github.com/MildyNora/NetworkSandbox/releases/latest/download/netsandbox-linux-x86_64.tar.gz)
- [SHA-256 checksums](https://github.com/MildyNora/NetworkSandbox/releases/latest/download/SHA256SUMS)

Each archive contains one ready-to-run `netsandbox` executable. No source build,
Rust toolchain, Docker, or virtual machine is required.

Technical and safety details are available in the
[reference documentation](docs/reference.md).
