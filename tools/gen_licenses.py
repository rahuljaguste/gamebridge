#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Generate THIRD_PARTY_LICENSES.md from Cargo metadata.

Usage:
    python3 tools/gen_licenses.py          # rewrite the report
    python3 tools/gen_licenses.py --check  # fail if the report is stale
"""

from __future__ import annotations

import json
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUTPUT = ROOT / "THIRD_PARTY_LICENSES.md"


def metadata() -> dict:
    command = ["cargo", "metadata", "--format-version", "1"]
    result = subprocess.run(
        [*command, "--offline"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        # A fresh checkout may not have the registry cache yet.
        result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
    if result.returncode != 0:
        raise SystemExit(result.stderr.strip() or "cargo metadata failed")
    return json.loads(result.stdout)


def license_of(package: dict) -> str:
    if package.get("license"):
        return package["license"]
    if package.get("license_file"):
        return f"see {package['license_file']}"
    return "UNKNOWN"


def render() -> str:
    meta = metadata()
    workspace = set(meta["workspace_members"])
    groups: dict[str, list[str]] = defaultdict(list)
    for package in meta["packages"]:
        if package["id"] in workspace:
            continue
        groups[license_of(package)].append(f"{package['name']} {package['version']}")

    lines = [
        "# Third-party license inventory",
        "",
        "Generated from `Cargo.lock` by `python3 tools/gen_licenses.py`.",
        "Do not edit by hand; re-run the script after changing dependencies.",
        "",
        "GameBridge itself is AGPL-3.0-only. The dependency licenses below are",
        "compatible with that license; this file is an inventory, not legal advice.",
        "",
    ]
    for name in sorted(groups, key=str.casefold):
        lines.append(f"## {name}")
        lines.append("")
        for crate in sorted(groups[name], key=str.casefold):
            lines.append(f"- {crate}")
        lines.append("")
    return "\n".join(lines).rstrip() + "\n"


def main() -> int:
    report = render()
    if "--check" in sys.argv[1:]:
        current = OUTPUT.read_text() if OUTPUT.exists() else ""
        if current != report:
            print(
                f"{OUTPUT.name} is stale; run python3 tools/gen_licenses.py",
                file=sys.stderr,
            )
            return 1
        return 0
    OUTPUT.write_text(report)
    print(f"wrote {OUTPUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
