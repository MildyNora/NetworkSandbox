# Network Sandbox

[English](README.md) | [简体中文](README.zh-CN.md)

[![Release](https://img.shields.io/github/v/release/MildyNora/NetworkSandbox?display_name=tag)](https://github.com/MildyNora/NetworkSandbox/releases/latest)
[![CI](https://github.com/MildyNora/NetworkSandbox/actions/workflows/ci.yml/badge.svg)](https://github.com/MildyNora/NetworkSandbox/actions/workflows/ci.yml)
[![Platforms](https://img.shields.io/badge/platforms-macOS%20%7C%20Linux-5865F2.svg)](#安装)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

**面向联网配置变更的轻量级智能体安全底座。**

Network Sandbox 可以让你的agent在处理网络相关问题的时侯，避免关掉自己的代理，或者其他已经建立的连接。
这是通过将任何关于网络相关的变更放置在一个沙箱内进行，Agent会确认在沙箱里面这些变更的配置不会影响当前的链接。
可以避免Agent自己修改网络导致自己断连，并且当前电脑的网络环境一团糟的情况。


原生命令行工具与智能体skill会一同安装。安装完成后，skill会告诉兼容的智能体
何时以及如何使用Network Sandbox，因此你无需亲自管理每一个安全步骤。

## 安装

```bash
brew install MildyNora/tap/network-sandbox
```

不使用 Homebrew：

```bash
curl -fsSL https://github.com/MildyNora/NetworkSandbox/releases/latest/download/install.sh | sh
```

两种方式都会安装 `netsandbox` 命令行工具及其智能体技能。该技能会被链接到
Codex 和通用智能体技能目录，使兼容的智能体自动遵循受保护的联网配置工作流。

无需从源码构建，也无需安装 Rust 工具链、Docker 或虚拟机。

直接下载：[macOS — Apple 芯片](https://github.com/MildyNora/NetworkSandbox/releases/latest/download/netsandbox-macos-arm64.tar.gz)
· [Linux — x86_64](https://github.com/MildyNora/NetworkSandbox/releases/latest/download/netsandbox-linux-x86_64.tar.gz)
· [校验和](https://github.com/MildyNora/NetworkSandbox/releases/latest/download/SHA256SUMS)

## 快速开始

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

## 工作原理

可以将 Network Sandbox 理解为专门用于高风险联网配置变更的 Anaconda
式环境。智能体会创建一个命名环境，在其中演练命令、检查必要连接并审阅计划，
最后才应用已经验证的差异。

在执行 `apply` 之前，真实主机不会被修改。`check` 会列出所有失败或无法验证的
连接：必要连接均保持正常时退出码为 `0`；连接验证阻止变更时为 `2`；发生操作
错误时为 `1`。应用后的验证一旦失败，变更会被自动回滚。

技术实现与安全细节请参阅[参考文档](docs/reference.md)。

## 关于

Network Sandbox 目前仍处于早期开发阶段。欢迎提交问题、反馈和拉取请求。

## 许可证

本项目采用 [Apache License 2.0](LICENSE)。
