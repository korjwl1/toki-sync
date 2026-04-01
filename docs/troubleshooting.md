# Troubleshooting

## Caddy fails to get TLS certificate

- Verify your DuckDNS subdomain points to the correct IP: visit [https://www.duckdns.org](https://www.duckdns.org) and check
- Verify `DUCKDNS_TOKEN` is correct in `.env`
- Check Caddy logs: `docker logs toki-caddy`
- Ensure ports 443 and 9090 are not blocked by your firewall
- Let's Encrypt has rate limits — if you exceeded them, wait and retry

---

## `toki settings sync enable` times out

- Verify the server is running: `docker compose ps`
- Verify port 9090 is reachable: `nc -zv yourserver.duckdns.org 9090`
- Check firewall rules on your server
- Check toki-sync-server logs: `docker logs toki-sync-server`

---

## "connection refused" or "certificate error"

- For self-signed TLS (Scenario C), use `--insecure` flag:
  ```bash
  toki settings sync enable --server <ip> --insecure
  ```
- For domain-based TLS (Scenario A), ensure DNS is propagated: `dig myserver.duckdns.org`
- Check that `TOKI_EXTERNAL_URL` in `.env` matches the actual domain/IP

---

## Event store issues

**Fjall (default)**:
- Fjall is embedded -- no separate container to check. Look at toki-sync-server logs: `docker logs toki-sync-server`
- Ensure the `toki-data` volume has sufficient disk space
- If data appears corrupted, stop the server, delete `/data/events.fjall`, and restart. Clients will perform a full re-sync.

**ClickHouse (optional)**:
- Check logs: `docker logs toki-clickhouse`
- Check health: `docker exec toki-clickhouse wget -qO- http://localhost:8123/ping`
- Ensure the `clickhouse-data` volume has sufficient disk space

---

## Dashboard shows no data

- Verify at least one device is connected: `toki settings sync devices`
- Check that the toki daemon is running on the client: `toki daemon status`
- Verify sync status on the client: `toki settings sync status`
- Check server logs for errors: `docker logs toki-sync-server`

---

## "invalid credentials" on login

- Verify the password matches `TOKI_ADMIN_PASSWORD` in `.env`
- The admin account is created on first server start. If you changed the password in `.env` after first start, it does not update automatically. Use the API or dashboard to change it.

---

## Sync reconnection issues

- The toki daemon uses exponential backoff (2s to 300s cap) when disconnected
- Check client-side sync status: `toki settings sync status`
- Restart the toki daemon: `toki daemon restart`
- Check server logs for auth errors: `docker logs toki-sync-server`
