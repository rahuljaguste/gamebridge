#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (C) 2026 Rahul Pramod Jaguste and contributors
"""Extract the first FEC test case from Chiaki's fec_test_cases.inl."""
import re
import sys


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit("usage: gen_fec_vector.py <fec_test_cases.inl> <out.rs>")
    src = open(sys.argv[1], "r", encoding="utf-8", errors="replace").read()
    m = re.search(
        r"\{\s*(\d+)\s*,\s*(\d+)\s*,\s*\{([^}]*)\}\s*,\s*((?:\s*\"[^\"]*\")+?)\s*,\s*(\d+)\s*\}",
        src,
        re.S,
    )
    if not m:
        raise SystemExit("no case found")
    k, mm, eras, strings, unit = m.groups()
    erasures = [int(x.strip()) for x in eras.split(",") if x.strip() and int(x.strip()) >= 0]
    b64 = "".join(re.findall(r'"([^"]*)"', strings))
    out = [
        "// SPDX-License-Identifier: AGPL-3.0-only",
        "// Copyright (C) 2026 Rahul Pramod Jaguste and contributors",
        "// Generated from the Chiaki v2.2.0 FEC test suite by tools/gen_fec_vector.py.",
        f"pub const K: usize = {k};",
        f"pub const M: usize = {mm};",
        f"pub const UNIT_SIZE: usize = {unit};",
        f"pub const ERASURES: [usize; {len(erasures)}] = [{', '.join(map(str, erasures))}];",
        f'pub const FRAME_B64: &str = "{b64}";',
    ]
    open(sys.argv[2], "w", encoding="utf-8").write("\n".join(out) + "\n")


if __name__ == "__main__":
    main()
