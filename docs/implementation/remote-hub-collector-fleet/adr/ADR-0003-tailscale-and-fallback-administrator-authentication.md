# ADR-0003: Tailscale And Fallback Administrator Authentication

- Status: accepted
- Date: 2026-07-14
- Canonical stream: `dirtydash-px3`

See also: [`CONTEXT.md`](../CONTEXT.md), [`/api/v1` Protocol And Privacy Invariants`](../API_V1_INVARIANTS.md)

## Context

The Hub must support a private default deployment path without turning public reverse proxies into a confused-deputy trust boundary. It also needs an administrator login that still works when Tailscale is unavailable or intentionally not used.

## Decision

Dirtydash uses two explicit administrator trust modes:

- Tailscale Serve is the default private HTTPS entry point.
- Public or non-Tailscale listeners require fallback administrator authentication with Argon2id-backed credentials and normal browser session protections.
- Public listeners ignore Tailscale headers unless the Hub itself verified them at the private boundary.
- Collector credentials are independent from administrator credentials.

## Consequences

- The deployment UX can optimize for private-by-default setups without making Tailscale mandatory.
- Authentication code must keep Collector ingestion, Tailscale-derived administration, and fallback administrator sessions clearly separated.
- Security tests must cover forged Tailscale headers, CSRF/session behavior, and credential revocation.
- Documentation and product copy must describe Tailscale as the default, not the only, administrative access path.
