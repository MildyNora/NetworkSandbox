# Contributing

Network Sandbox is a functional preview. Contributions should preserve its
fail-closed behavior and must not weaken required connectivity checks or
rollback protection.

## Development

Use Rust 1.85 or newer and run:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

Changes to Linux namespaces, macOS route transactions, rollback, filesystem
promotion, or application canaries should include a regression test. Never use
real production routes, credentials, or remotely irreplaceable hosts in tests.

## Pull requests

Keep changes focused, explain the safety invariant affected, and describe the
validation performed. Report security issues privately as directed in
`SECURITY.md`.
