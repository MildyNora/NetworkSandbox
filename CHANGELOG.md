# Changelog

## 0.6.1 — 2026-07-28

- Refresh required circuits after administrator authorization and before the
  final apply plan.
- Permit narrowly scoped `/dev/null` access in native macOS execution so SSH
  and standard Unix canaries work normally.
- Add guarded macOS route trials with transactional verification and automatic
  rollback.
- Update the bundled Codex skill and preflight to require compatible behavior.

## 0.6.0 — 2026-07-28

- Add explicit guarded route-trial planning for macOS.
- Defer route-dependent checks that cannot be represented by per-socket
  interface binding.
- Preserve unrelated control circuits as mandatory pre-apply gates.
