## Summary

What changed and why?

## Testing

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `node --check crates/server/src/static/app.js`
- [ ] `node --check crates/server/src/static/motion-math.js`
- [ ] `node --test tools/test_motion_math.js`
- [ ] `python3 tools/gen_licenses.py --check`

Describe any hardware, console, browser, or network testing. Note anything
that could not be tested.

## Checklist

- [ ] The change is compatible with AGPL-3.0-only and contains no copied Sony
      SDK, firmware, artwork, or extracted keys.
- [ ] No `hosts.json`, auth tokens, PINs, PSN account IDs, registration keys,
      or personal data are included.
- [ ] User-visible behavior is documented in `README.md` and/or `CHANGELOG.md`.
- [ ] Protocol or session behavior has tests; no test requires a real console,
      PSN account, or network access.
- [ ] Security vulnerabilities are being reported privately, not in this PR.
