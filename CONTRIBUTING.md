# Contributing

## License

Setu is licensed **AGPL-3.0-only** and is a derivative work of Chiaki v2.2.0.
By contributing, you agree that your contributions are licensed under
AGPL-3.0-only. The project is permanently AGPL-3.0-only as a Chiaki derivative
and cannot be relicensed permissively.

## Provenance rules

Do not submit:

- code, SDK headers, or constants copied from a Sony SDK or decompiled Sony
  software;
- firmware dumps, extracted keys, or Sony artwork/assets;
- anything you do not have the right to license under AGPL-3.0-only.

Protocol constants must come from Chiaki's public AGPL source or public
documentation, and their provenance must be noted in a comment.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python3 tools/gen_licenses.py --check
```

Run the bridge with `cargo run -p setu-server -- --demo-file demo.h264` to test
the browser path without a console. Tests must not require a real PSN account,
console, or network access.

Do not commit `hosts.json`, auth tokens, `server.log`, console captures, or
anything else containing registration keys or personal data.

## Pull requests

Keep changes focused, add tests for protocol or session behavior, and update
the README or `CHANGELOG.md` when user-visible behavior changes. Security issues
go through [SECURITY.md](SECURITY.md), not a public issue.
