# Backup & Restore

## Data Volumes

| Volume | Path | Contents | On loss |
|--------|------|----------|---------|
| `toki-data` | `/data` | SQLite (users, devices, cursors) + Fjall event store | Re-login + full re-sync required |
| `clickhouse-data` | `/var/lib/clickhouse` | ClickHouse event data (only with `--profile clickhouse`) | Recoverable via client re-sync |
| `caddy-data` | `/data` | Let's Encrypt certificates | Auto-reissue (rate limit: 5/week) |

With the default Fjall backend, `toki-data` contains both metadata and events. If lost, clients will perform a full re-sync from their local history on reconnect. With ClickHouse, event data is stored separately in `clickhouse-data`.

---

## Bind Mounts (recommended for backups)

For easier backup access, use bind mounts instead of named volumes in `docker-compose.yml`:

```yaml
volumes:
  - ./data/toki:/data
```

---

## Fjall Backup (default backend)

Fjall stores data as files in a directory. To back up:

```bash
# Stop containers to ensure consistency
docker compose down

# Archive the data directory
tar czf toki-sync-backup-$(date +%Y%m%d).tar.gz ./data/

# Restart
docker compose --profile caddy up -d
```

Alternatively, for a hot backup (server running), copy the Fjall directory. Fjall uses an LSM-tree structure that is safe to copy while running, though stopping the server ensures full consistency.

---

## ClickHouse Backup (optional backend)

If using ClickHouse as the event store:

```bash
# Use clickhouse-backup tool
docker exec toki-clickhouse clickhouse-backup create backup_$(date +%Y%m%d)

# Or use clickhouse-client to export
docker exec toki-clickhouse clickhouse-client --query "SELECT * FROM events FORMAT Native" > events_backup.bin
```

See [ClickHouse backup documentation](https://clickhouse.com/docs/en/operations/backup) for full details.

---

## VM / VPS Disk Snapshots

The simplest approach for small deployments:

1. Stop containers: `docker compose down`
2. Snapshot the entire VM/VPS disk via your cloud provider's console
3. Restart: `docker compose --profile caddy up -d`

This captures everything -- database, event store, and certificates.

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
