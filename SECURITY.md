# Security

Network Sandbox 0.6.1 is a preview and must not be treated as a security boundary
against malicious root code. It is intended to protect against accidental
administrative changes while its isolation and transaction model is hardened.

Do not use this version on a production host that lacks an independent recovery
path. Run the repository's disposable Linux-host and macOS Linux-image
end-to-end scenarios first.

Sensitive environment metadata and rollback backups should be stored on a
root-owned filesystem. They can contain copies of credentials changed during an
experiment.

Report path traversal, namespace escape, rollback corruption, state tampering,
credential exposure, or unintended host-network mutation through
[GitHub private vulnerability reporting](https://github.com/MildyNora/NetworkSandbox/security/advisories/new).
Do not disclose suspected vulnerabilities in a public issue.
