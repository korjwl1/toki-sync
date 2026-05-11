# Troubleshooting

Use this page to map a symptom to a section below. First diagnostic commands for any issue: `docker compose ps` (are containers up?) and `docker logs <container-name>` (what's the error?).

## Symptom map

| Symptom | Likely cause | Section |
|---|---|---|
| Caddy fails to start, no HTTPS | DuckDNS misconfig, blocked port 443, Let's Encrypt rate limit | [Caddy fails to get TLS certificate](#caddy-fails-to-get-tls-certificate) |
| `toki settings sync enable` hangs or times out | Server down, port 9090 blocked, wrong host | [`toki settings sync enable` times out](#toki-settings-sync-enable-times-out) |
| TLS handshake fails, "certificate error" on connect | Self-signed without `--insecure`, DNS not propagated | ["connection refused" or "certificate error"](#connection-refused-or-certificate-error) |
| Dashboard loads but shows no usage | No device connected, daemon not running on client, sync stalled | [Dashboard shows no data](#dashboard-shows-no-data) |
| Login rejected on web/CLI | Wrong admin password, password change not reflected in `.env` | ["invalid credentials" on login](#invalid-credentials-on-login) |
| Event-store errors in logs | Fjall directory corrupted, ClickHouse down, disk full | [Event store issues](#event-store-issues) |
| Sync disconnects repeatedly | Network instability, server restart loop, auth expired | [Sync reconnection issues](#sync-reconnection-issues) |

For deployment-specific quirks, see the four deployment guides: [Caddy + DuckDNS](deploy-caddy-duckdns.md) (Scenario A), [existing reverse proxy](deploy-reverse-proxy.md) (Scenario B), [self-signed TLS](deploy-self-signed.md) (Scenario C), [local / LAN](deploy-local.md) (Scenario D).

---

## Caddy fails to get TLS certificate

Applies to Scenario A (Caddy + DuckDNS).

- Verify your DuckDNS subdomain points to the correct IP at [https://www.duckdns.org](https://www.duckdns.org).
- Verify `DUCKDNS_TOKEN` is correct in `.env`.
- Check Caddy logs: `docker logs toki-caddy`.
- Ensure ports 443 and 9090 are not blocked by your firewall.
- Let's Encrypt enforces a limit of 5 duplicate certificate issuances per week per domain — if you exceeded it, wait and retry.

---

## `toki settings sync enable` times out

- Verify the server is running: `docker compose ps`.
- Verify port 9090 is reachable: `nc -zv yourserver.duckdns.org 9090`.
- Check firewall rules on your server.
- Check toki-sync-server logs: `docker logs toki-sync-server`.

---

## "connection refused" or "certificate error"

- For self-signed TLS (Scenario C), use the `--insecure` flag:

  ```bash
  toki settings sync enable --server <ip> --insecure
  ```

- For domain-based TLS (Scenario A), ensure DNS is propagated: `dig myserver.duckdns.org`.
- Check that `TOKI_EXTERNAL_URL` in `.env` matches the actual domain or IP.

---

## Event store issues

### Fjall (default)

- Fjall is embedded — no separate container to check. Look at the server logs: `docker logs toki-sync-server`.
- Ensure the `toki-data` volume has sufficient disk space.
- If data appears corrupted, stop the server, delete `/data/events.fjall`, and restart. Clients will perform a full re-sync.

### ClickHouse (optional)

- Check logs: `docker logs toki-clickhouse`.
- Check health: `docker exec toki-clickhouse wget -qO- http://localhost:8123/ping`.
- Ensure the `clickhouse-data` volume has sufficient disk space.

---

## Dashboard shows no data

- Verify at least one device is connected: `toki settings sync devices`.
- Check that the toki daemon is running on the client: `toki daemon status`.
- Verify sync status on the client: `toki settings sync status`.
- Check server logs for errors: `docker logs toki-sync-server`.

---

## "invalid credentials" on login

- Verify the password matches `TOKI_ADMIN_PASSWORD` in `.env`.
- The admin account is created on first server start. Changing `TOKI_ADMIN_PASSWORD` in `.env` after that does not update the stored hash. Use the API or dashboard to change the password.

---

## Sync reconnection issues

- The toki daemon uses exponential backoff (2s to 300s cap) when disconnected.
- Check client-side sync status: `toki settings sync status`.
- Restart the toki daemon: `toki daemon restart`.
- Check server logs for auth errors: `docker logs toki-sync-server`.
