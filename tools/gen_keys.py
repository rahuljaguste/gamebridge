#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (C) 2026 Rahul Pramod Jaguste and contributors
"""Extract the rpcrypt key tables from Chiaki's rpcrypt.c into a Rust source file.

Run with: python3 -I tools/gen_keys.py <rpcrypt.c> <out.rs>
The input is treated as data only.
"""
import re
import sys


def extract(source: str, name: str, expected: int) -> list[int]:
    m = re.search(r"static const uint8_t\s+" + re.escape(name) + r"\s*\[[^\]]*\]\s*=\s*\{(.*?)\};", source, re.S)
    if not m:
        raise SystemExit(f"array {name} not found")
    body = re.sub(r"//.*", "", m.group(1))
    body = re.sub(r"/\*.*?\*/", "", body, flags=re.S)
    values = [int(x, 16) for x in re.findall(r"0x([0-9a-fA-F]{1,2})", body)]
    if len(values) != expected:
        raise SystemExit(f"{name}: expected {expected} bytes, found {len(values)}")
    return values


def fmt_rows(values: list[int], row_len: int, keep: int) -> str:
    rows = []
    for i in range(0, len(values), row_len):
        chunk = values[i:i + row_len][:keep]
        rows.append("    [" + ", ".join(f"0x{b:02x}" for b in chunk) + "],")
    return "\n".join(rows)


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit("usage: gen_keys.py <rpcrypt.c> <out.rs>")
    source = open(sys.argv[1], "r", encoding="utf-8", errors="replace").read()

    a_names = ["keys_a_ps4", "keys_a_ps5", "keys_b_ps4", "keys_b_ps5"]
    zero_names = ["ps4_keys_0", "ps5_keys_0", "ps4_keys_1", "ps5_keys_1"]

    parts = [
        "// SPDX-License-Identifier: AGPL-3.0-only",
        "// Copyright (C) 2026 Rahul Pramod Jaguste and contributors",
        "// Generated from Chiaki v2.2.0 lib/src/rpcrypt.c by tools/gen_keys.py.",
        "// Do not edit by hand. The table layout mirrors the reference implementation.",
        "",
    ]
    for name in a_names:
        vals = extract(source, name, 0x70 * 0x20)
        parts.append(f"pub const {name.upper()}: [[u8; 0x10]; 0x20] = [")
        parts.append(fmt_rows(vals, 0x70, 0x10))
        parts.append("];")
        parts.append("")
    for name in zero_names:
        vals = extract(source, name, 0x200)
        parts.append(f"pub const {name.upper()}: [[u8; 0x20]; 16] = [")
        parts.append(fmt_rows(vals, 0x20, 0x20))
        parts.append("];")
        parts.append("")

    with open(sys.argv[2], "w", encoding="utf-8") as f:
        f.write("\n".join(parts))


if __name__ == "__main__":
    main()
