# Security policy

## Supported versions

Security fixes are applied to the latest release on `main`.

## Reporting a vulnerability

Please report vulnerabilities privately through GitHub's **Security → Report a
vulnerability** flow (Security Advisories) on this repository. Do not open a
public issue for a vulnerability.

Include the affected version or commit, the impact, reproduction steps or a
proof of concept, and any suggested fix. Please allow time for a fix before
public disclosure.

## Sensitive data

Treat the following as credentials:

- `hosts.json` in the configuration directory, which contains the console
  RegistKey and RP-Key.
- The access token passed with `--auth-token` or `--auth-token-file`.
- Console PINs, screen contents, and controller input.

Do not paste any of these into a public issue, and redact them from logs. The
server does not intentionally log registration keys or the auth token; report
any code path that does.

## Scope

In scope: the Rust bridge, protocol parsing, authentication and origin checks,
the HTTP/WebSocket API, and the browser UI.

Out of scope: Sony's console firmware, the official Remote Play application,
and vulnerabilities in third-party dependencies. Report dependency issues
upstream, but tell us if the pin is affected.
