# Backup & Restore

## Data Volumes

| Volume | Path | Contents | On loss |
|--------|------|----------|---------|
| `toki-data` | `/data` | SQLite (users, devices, cursors) | Re-login + full re-sync required |
| `vm-data` | `/vm-data` | VictoriaMetrics time-series data | **Unrecoverable** — all historical usage data is lost |
| `caddy-data` | `/data` | Let's Encrypt certificates | Auto-reissue (rate limit: 5/week) |

`vm-data` is the critical volume. If lost, all historical token usage data is gone permanently.

---

## Bind Mounts (recommended for backups)

For easier backup access, use bind mounts instead of named volumes in `docker-compose.yml`:

```yaml
volumes:
  - ./data/vm:/vm-data
  - ./data/toki:/data
```

---

## VictoriaMetrics Hot Snapshots

VictoriaMetrics supports creating snapshots without downtime:

```bash
# Create snapshot
docker exec victoriametrics wget -qO- http://localhost:8428/snapshot/create

# List snapshots
docker exec victoriametrics wget -qO- http://localhost:8428/snapshot/list

# Delete a snapshot
docker exec victoriametrics wget -qO- "http://localhost:8428/snapshot/delete?snapshot=SNAPSHOT_NAME"
```

Snapshots are stored under the `vm-data` volume in a `snapshots/` directory.

See [VictoriaMetrics backup docs](https://docs.victoriametrics.com/single-server-victoriametrics/#backups) for full details.

---

## VM / VPS Disk Snapshots

The simplest approach for small deployments:

1. Stop containers: `docker compose down`
2. Snapshot the entire VM/VPS disk via your cloud provider's console
3. Restart: `docker compose --profile caddy up -d`

This captures everything — database, time-series, and certificates.

---

## Manual File Backup

If using bind mounts:

```bash
# Stop containers to ensure consistency
docker compose down

# Archive data
tar czf toki-sync-backup-$(date +%Y%m%d).tar.gz ./data/

# Restart
docker compose --profile caddy up -d
```

---

## Restore

1. Stop containers: `docker compose down`
2. Replace the data directories with your backup
3. Restart: `docker compose --profile caddy up -d`
4. Clients will reconnect automatically (toki daemon retries with exponential backoff)
