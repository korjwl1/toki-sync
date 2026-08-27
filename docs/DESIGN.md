# toki-sync architecture and design

This document describes the code on the current branch. It intentionally does
not describe planned VictoriaMetrics, PromQL-proxy, built-in usage-dashboard,
or multi-replica features as if they existed.

## Implemented topology

```text
toki daemon(s)
    | plain TCP :9090 inside the deployment
    | bincode frames, optional zstd batch payloads
    v
toki-sync process
    |-- relational repository: SQLite (default) or PostgreSQL
    |-- event repository: Fjall (default) or ClickHouse
    |-- HTTP :9091
        |-- authentication/device-code/OIDC
        |-- /api/v1/toki/query
        |-- user/admin/team APIs
        +-- optional toki_monitor settings channel

TLS terminator (deployment responsibility)
    |-- HTTPS -> :9091
    +-- TLS TCP -> :9090
```

The process itself listens with plain TCP and HTTP. The bundled Caddy profile
can terminate TLS with its internal CA; an external reverse proxy can provide a
public certificate. The current Caddyfile does not implement the documented
DuckDNS DNS challenge.

`/admin` is a user/device/team/settings administration console. There is no
built-in token-usage chart route. Usage consumers call the query API or use a
separate app such as Toki Monitor.

## Runtime tasks and concurrency

- one Tokio task accepts HTTP connections;
- one listener task accepts TCP sync connections;
- each TCP client gets one handler task;
- a semaphore rejects connections beyond 500;
- a configurable semaphore (default 10) bounds simultaneous EventStore batch
  writes;
- blocking Fjall, ClickHouse/ureq, bcrypt, and pricing work is moved to blocking
  tasks where the implementation does so;
- shutdown stops accepting TCP clients and waits up to 30 seconds before
  aborting remaining handler tasks;
- cleanup and pricing refresh run every six hours.

This is a single-process design today. Fjall is embedded, and the SQLite path
is a local file. ClickHouse window merge locks and monitor/window rate limiters
are process-local. Running multiple toki-sync replicas against the same stores
is not documented as safe and has no distributed coordination test.

## Sync protocol

Frames are:

```text
[message type: u32 little endian]
[payload length: u32 little endian]
[bincode payload bytes]
```

The protocol constants in the sibling crate are protocol version 1, event
schema version 3, and a 16 MiB frame payload maximum. Before authentication,
the server additionally limits the first frame to 64 KiB and requires it within
10 seconds. Normal connection reads time out after 120 seconds.

Implemented message types:

| Group | Messages |
|---|---|
| Authentication | `AUTH`, `AUTH_OK`, `AUTH_ERR` |
| Cursor | `GET_LAST_TS`, `LAST_TS` |
| Events | `SYNC_BATCH`, `SYNC_BATCH_ZSTD`, `SYNC_ACK`, `SYNC_ERR` |
| Windows | `SYNC_WINDOWS` (requires `sync_windows_v1` capability) |
| Keepalive | `PING`, `PONG` |

Bincode structs are field-order sensitive. Unknown message discriminants are a
connection error, so clients must call public `GET /api/v1/capabilities` before
sending optional messages.

### Event batch flow

1. Client authenticates with JWT, stable device key, provider, protocol
   version, and schema version.
2. Server resolves or creates a device for that user and provider cursor.
3. Client asks for `last_ts_ms` and uploads later local events.
4. The server validates dictionary references and token columns, resolves wire
   events, and limits a decoded batch to 50,000 items.
5. The EventStore write completes before the relational cursor advances.
6. `SYNC_ACK` reports the server cursor.

A zstd frame may expand to at most 64 MiB. The maximum windows payload is 1
MiB and at most 2,000 newest window items are considered. Window batches are
rate-limited per user/provider to one accepted batch per minute.

If protocol versions differ, authentication fails. If event schema versions
differ, the server purges that device's remote events, resets its provider
cursor, and asks the client for a full re-sync.

### Cursor and deduplication invariants

The relational cursor key is `(device_id, provider)`. Event upsert is intended
to be idempotent by `(device_id, provider, msg_id)`. Fjall keeps a secondary
dedup index with that key and the newest timestamp wins. A fresh ClickHouse
table uses `ReplacingMergeTree(ts_ms)` with
`ORDER BY (device_id, provider, msg_id)` and queries use `FINAL`.

This is idempotent replay, not a transactional exactly-once protocol: an
EventStore write can succeed before a cursor update fails, after which the
client resends and deduplication converges the result.

Fjall's dedup-index entries are pruned after `dedup_retention_secs` (30 days by
default); event records are not deleted by that cleanup.

## Relational storage

`DatabaseRepo` has SQLite and PostgreSQL implementations for users, devices,
cursors, refresh tokens, device codes, pending registrations, teams, dynamic
settings, user activity, and monitor settings.

SQLite opens in WAL mode with foreign keys enabled and normal synchronous mode.
Monitor-setting compare-and-swap/quota writes acquire a write transaction before
reading state. PostgreSQL uses SQL transactions and row locking in the
corresponding paths.

Migrations are startup DDL embedded in each implementation. There is no shared
`meta` schema-version table for the relational repositories. SQLite performs
its cursor-table rebuild and additive `ALTER TABLE` statements; PostgreSQL uses
`CREATE TABLE IF NOT EXISTS` and `ADD COLUMN IF NOT EXISTS` where implemented.

The current test suite drives many HTTP and monitor paths through a temporary
real SQLite database. It does not connect to PostgreSQL, so parity and migration
claims for PostgreSQL remain unverified by automated integration tests.

## Event storage

### Fjall

Fjall stores event rows plus message, user, and session indexes. The global
event key starts with big-endian `ts_ms`, so all-user scans seek directly to the
requested start. A single-user scan uses a user-prefix index but currently
starts at the beginning of that user's history and skips rows older than
`since_ms`; it is therefore O(total history for that user) even for a recent
query. Team scans concatenate members in member-list order rather than merging
their streams globally by time.

Fjall records an event schema version. An incompatible persisted version clears
the event keyspaces and relies on client re-sync. This is separate from the wire
schema version and relational DDL.

### ClickHouse

ClickHouse creates `toki_events` and `toki_windows` at startup. Event queries
use `FINAL`, time/user predicates, `ORDER BY ts_ms`, and a caller-provided
`LIMIT`. The synchronous HTTP adapter is run in a blocking task.

Two upgrade paths must not be confused:

- `toki_windows` has an automatic rebuild migration from
  `ReplacingMergeTree(observed_ts_ms)` to an `updated_at` version column, with
  interrupted-rename recovery;
- `toki_events` has an additive `session` column migration, but **no** migration
  from the old `(device_id, msg_id)` sort key to
  `(device_id, provider, msg_id)`. Operators must inspect and rebuild old tables
  before mixing providers.

Neither ClickHouse creation/migration path is exercised against a live server
by this repository's tests.

## Rate-limit windows

Window rows are account-level, not device-level. Their key is
`(user, provider, limit_id, account, kind, window_end_ms)`. Clients resend their
full recent set because a fixed window changes over time. The server merges
fields monotonically where possible (peak=max, flags=OR, first-seen=min) and
uses the most recent observation for live fields.

`active_ms`, sample count, and sampled-active fraction are merged by `max`, not
recomputed from the cross-device event union. This is an intentional current
approximation and may undercount multi-device activity. Windows queries support
only `scope=self`; rows older than 730 days are cleaned periodically.

Fjall serializes window mutation in process. ClickHouse uses per-user
process-local locks around read/merge/write. Multiple server replicas could race
and lose a contribution until a client later resends; there is no distributed
lock.

## HTTP query execution

`GET /api/v1/toki/query` accepts a strict subset described in [API.md](API.md).
EventStore ranges are half-open `[start_ms, end_ms)`. Date-only end values are
converted to the next local midnight; exact numeric/date-time ends are
exclusive. RFC 3339 and dashed dates are not accepted remotely.

Resource guards:

| Guard | Current value | Important limitation |
|---|---:|---|
| Time buckets | 2,000 | Too-small `step` is rejected |
| Input events | 200,000 | Hard cap, no cursor; aggregate truncation is not propagated |
| Aggregate groups | 50,000 | Adds top-level `truncated` when new groups are dropped |
| ClickHouse buffered body | 10 MiB | Can return `502` before the event cap |

Bare raw `events` reports event-cap truncation at the response top level. The
current toki CLI conversion drops that flag. Aggregates do not receive the
event-cap flag, so their totals can silently be partial. The cap bounds some
memory use but is not pagination or response streaming.

## Authentication and security

- access and refresh tokens are signed JWTs;
- refresh tokens rotate once and are stored by ID/revocation state;
- password changes revoke all of the user's refresh tokens;
- password, registration, device-code, and refresh paths use brute-force/rate
  guards where implemented;
- inactive users are rejected by HTTP and TCP paths through a shared short-TTL
  cache;
- query scope is resolved from the authenticated subject, team membership, role,
  and dynamic maximum scope;
- monitor settings are opaque per-user strings with versioned CAS, per-entry and
  total quotas, and a process-local write budget;
- prompts and model responses are not part of `ServerEvent` or sync wire items.

The service does not terminate TLS itself. Trust proxy headers only when
`trust_proxy=true` and the direct peer is a controlled proxy.

## Failure and recovery properties

- client history is the recovery source for remote event loss;
- server writes events before advancing cursors, so a crash causes replay rather
  than acknowledged data loss;
- device/account deletion attempts event/window purges before and after deleting
  relational ownership rows to catch in-flight writes;
- switching Fjall and ClickHouse does not migrate data;
- losing relational users/devices/cursors requires reauthentication and may
  require explicit cursor reset/full re-sync;
- ClickHouse mutations are asynchronous at the database level and must be
  included in an operator's backup/recovery plan.

## Validation and release status

At commit `5804fc2`, Rust/Cargo 1.92.0 lists 116 tests. Coverage includes
protocol framing/handlers, parsing/aggregation, pricing, Fjall, and HTTP/monitor
paths with temporary SQLite. There are zero live PostgreSQL and ClickHouse
integration tests, no Compose integration test, and no real ClickHouse migration
test.

The Cargo package and latest repository tag are 2.1.0. This branch additionally
uses `toki-sync-protocol` 1.1.0 source through a sibling patch, while the latest
protocol tag is v1.0.0. Consequently `docker build .` currently fails because
the sibling is outside its build context. Tag protocol v1.1.0, update consumers,
remove local patches, and build/test the image before treating this branch as a
releasable Docker source tree.
