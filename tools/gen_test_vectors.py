#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Extract byte/uint64 constants from one C test function into Rust consts.

Usage: python3 -I tools/gen_test_vectors.py <test.c> <function> <out.rs>
The C file is treated strictly as data.
"""
import re
import sys


def function_body(source: str, name: str) -> str:
    start = source.find(f"test_{name}(")
    if start < 0:
        start = source.find(name + "(")
    if start < 0:
        raise SystemExit(f"function {name} not found")
    brace = source.find("{", start)
    depth = 0
    for i in range(brace, len(source)):
        if source[i] == "{":
            depth += 1
        elif source[i] == "}":
            depth -= 1
            if depth == 0:
                return source[brace + 1:i]
    raise SystemExit("unbalanced function")


def main() -> None:
    if len(sys.argv) != 4:
        raise SystemExit("usage: gen_test_vectors.py <test.c> <function> <out.rs>")
    source = open(sys.argv[1], "r", encoding="utf-8", errors="replace").read()
    body = function_body(source, sys.argv[2])
    out = ["// Generated from the Chiaki v2.2.0 test suite by tools/gen_test_vectors.py."]
    for m in re.finditer(
        r"(?:static\s+)?(?:const\s+)?uint8_t\s+(\w+)\s*\[\]\s*=\s*\{(.*?)\};",
        body,
        re.S,
    ):
        name, raw = m.group(1), m.group(2)
        raw = re.sub(r"//.*", "", raw)
        vals = re.findall(r"0x[0-9a-fA-F]+|\b\d+\b", raw)

        def parse(v: str) -> int:
            if v.lower().startswith("0x"):
                return int(v, 16)
            if len(v) > 1 and v.startswith("0"):
                return int(v, 8)
            return int(v, 10)

        nums = [parse(v) for v in vals]
        out.append(f"pub const {name.upper()}: [u8; {len(nums)}] = [")
        for i in range(0, len(nums), 12):
            out.append("    " + ", ".join(f"0x{x:02x}" for x in nums[i:i + 12]) + ",")
        out.append("];")
    for m in re.finditer(
        r"(?:static\s+)?(?:const\s+)?uint64_t\s+(\w+)\s*=\s*([^;]+);", body
    ):
        name, expr = m.group(1), m.group(2).strip()
        expr = re.sub(r"\((?:uint64_t|unsigned long long)\)", "", expr)
        expr = re.sub(r"(\d+)[uU][lL][lL]", r"\1u64", expr)
        expr = re.sub(r"(\d+)[lL][lL]", r"\1u64", expr)
        expr = re.sub(r"\(\((.*?)\)\)", r"(\1)", expr)
        out.append(f"pub const {name.upper()}: u64 = {expr};")
    with open(sys.argv[3], "w", encoding="utf-8") as f:
        f.write("\n".join(out) + "\n")


if __name__ == "__main__":
    main()
