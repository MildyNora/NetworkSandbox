#!/usr/bin/env python3
"""Read-only Network Sandbox installation and backend preflight."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys

MINIMUM_VERSION = (0, 6, 1)


def find_binary(explicit: str | None) -> Path | None:
    candidates: list[Path] = []
    if explicit:
        candidates.append(Path(explicit).expanduser())
    discovered = shutil.which("netsandbox")
    if discovered:
        candidates.append(Path(discovered))
    candidates.append(Path.home() / ".local" / "bin" / "netsandbox")
    repository = Path(__file__).resolve().parents[3]
    candidates.append(repository / "target" / "release" / "netsandbox")
    for candidate in candidates:
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return candidate.resolve()
    return None


def run(binary: Path, *arguments: str) -> tuple[int, str, str]:
    completed = subprocess.run(
        [str(binary), *arguments],
        check=False,
        capture_output=True,
        text=True,
        timeout=20,
    )
    return completed.returncode, completed.stdout.strip(), completed.stderr.strip()


def supported_version(output: str) -> bool:
    match = re.fullmatch(r"netsandbox (\d+)\.(\d+)\.(\d+)", output.strip())
    return match is not None and tuple(map(int, match.groups())) >= MINIMUM_VERSION


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", help="Explicit netsandbox executable")
    parser.add_argument("--json", action="store_true", help="Emit machine-readable output")
    arguments = parser.parse_args()

    binary = find_binary(arguments.binary)
    if binary is None:
        result = {
            "available": False,
            "reason": "netsandbox executable was not found",
        }
        if arguments.json:
            print(json.dumps(result, indent=2))
        else:
            print("FAIL  netsandbox executable was not found")
        return 2

    version_code, version, version_error = run(binary, "--version")
    doctor_code, doctor, doctor_error = run(binary, "doctor")
    version_supported = version_code == 0 and supported_version(version)
    result = {
        "available": version_code == 0,
        "binary": str(binary),
        "version": version or version_error,
        "minimumVersion": ".".join(map(str, MINIMUM_VERSION)),
        "versionSupported": version_supported,
        "doctorExit": doctor_code,
        "doctor": doctor,
        "doctorError": doctor_error,
        "ready": version_supported and doctor_code == 0,
    }
    if arguments.json:
        print(json.dumps(result, indent=2))
    else:
        print(f"{'PASS' if version_supported else 'FAIL'}  {result['version']}")
        if version_code == 0 and not version_supported:
            print(f"      Network Sandbox {result['minimumVersion']} or newer is required")
        if doctor:
            print(doctor)
        if doctor_error:
            print(doctor_error, file=sys.stderr)
        print(f"{'PASS' if result['ready'] else 'FAIL'}  backend preflight")
    return 0 if result["ready"] else 2


if __name__ == "__main__":
    raise SystemExit(main())
