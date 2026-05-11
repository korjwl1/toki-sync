# Deployment guide

Pick one scenario based on your existing infrastructure, then follow the linked guide. All scenarios use the same `docker-compose.yml`; they differ in which profile is enabled and how TLS is handled.

## Decision flow

- Do you already run nginx, Traefik, or another reverse proxy? → [Scenario B: existing reverse proxy](deploy-reverse-proxy.md).
- Do you have a public IP but no domain? → [Scenario C: self-signed TLS](deploy-self-signed.md).
- Do you want a free domain with automatic Let's Encrypt? → [Scenario A: Caddy + DuckDNS](deploy-caddy-duckdns.md).
- Are you just running it on localhost for development? → [Scenario D: local / LAN](deploy-local.md).

## Comparison

| Scenario | TLS | Domain | Reverse proxy | Best for |
|---|---|---|---|---|
| [A — Caddy + DuckDNS](deploy-caddy-duckdns.md) | Auto (Let's Encrypt) | Free DuckDNS subdomain | Built-in Caddy | Fresh public server, no existing proxy |
| [B — Existing reverse proxy](deploy-reverse-proxy.md) | Handled by your proxy | Your own domain | nginx, Traefik, etc. | Servers that already terminate TLS |
| [C — Self-signed TLS](deploy-self-signed.md) | Self-signed (Caddy `tls internal`) | None (IP only) | Built-in Caddy | Home lab, LAN-only deployments |
| [D — Local / LAN](deploy-local.md) | None | None | None | Development and testing only |

## Operating the deployment

After deploying, refer to these:

- [Backup and restore](backup.md) — volume layout, cold backup with `tar`, ClickHouse backup, restore.
- [Troubleshooting](troubleshooting.md) — symptom-to-section mapping for TLS, sync, dashboard, and auth issues.
- [Configuration reference](CONFIGURATION.md) — all TOML keys, defaults, and environment variables.
