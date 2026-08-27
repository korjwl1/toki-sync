# Deployment guide

Pick a scenario based on your infrastructure. The bundled Caddy profile has an
important constraint:

1. The bundled Caddyfile forces Caddy's internal CA. `DUCKDNS_TOKEN` is unused,
   so the repository does not currently provide automatic public DuckDNS/
   Let's Encrypt certificates.

## Decision flow

- Do you already run nginx, Traefik, or another reverse proxy? → [Scenario B: existing reverse proxy](deploy-reverse-proxy.md).
- Do you have a public IP but no domain? → [Scenario C: self-signed TLS](deploy-self-signed.md).
- Do you want DuckDNS + public Let's Encrypt? → [Scenario A status and required external TLS work](deploy-caddy-duckdns.md); it is not turnkey in this revision.
- Are you just running it on localhost for development? → [Scenario D: local / LAN](deploy-local.md).

## Comparison

| Scenario | TLS | Domain | Reverse proxy | Best for |
|---|---|---|---|---|
| [A — Caddy + DuckDNS](deploy-caddy-duckdns.md) | Not wired in bundled Caddy | Free DuckDNS subdomain | Corrected/custom proxy required | Not turnkey in this revision |
| [B — Existing reverse proxy](deploy-reverse-proxy.md) | Handled by your proxy | Your own domain | nginx, Traefik, etc. | Servers that already terminate TLS |
| [C — Self-signed TLS](deploy-self-signed.md) | Internal-CA cert (Caddy `tls internal`) | IP/host | Built-in Caddy | Home lab, LAN-only deployments |
| [D — Local / LAN](deploy-local.md) | None | None | None | Development and testing only |

The server process always speaks plain TCP/HTTP on its bound ports. Never
publish Scenario D directly to the Internet.

## Operating the deployment

After deploying, refer to these:

- [Backup and restore](backup.md) — volume layout, cold backup with `tar`, ClickHouse backup, restore.
- [Troubleshooting](troubleshooting.md) — symptom-to-section mapping for TLS, sync, dashboard, and auth issues.
- [Configuration reference](CONFIGURATION.md) — all TOML keys, defaults, and environment variables.
