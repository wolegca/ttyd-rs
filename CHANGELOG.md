# Changelog

## 1.0.0 — 2026-08-25

### Security

- Refuse to start an unauthenticated terminal on a non-loopback address.
  Configure `[auth]`, bind to `127.0.0.1` / `::1`, or explicitly set
  `allow_unauthenticated = true` only when a trusted reverse proxy enforces
  authentication.
- Keep unauthenticated file transfer behind its separate explicit opt-in.

### Release readiness

- Update project, protocol, configuration, and roadmap documentation for
  version 1.0.0.
- Fix the invalid-PTY-FD regression test so cleanup never signals PID 1.

### Upgrade note

Deployments intentionally serving an unauthenticated terminal on `0.0.0.0`
or another non-loopback address must add `allow_unauthenticated = true` to
their configuration after independently confirming that a trusted proxy is
the authentication boundary.
