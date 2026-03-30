# toki-sync Architecture & Design

## Overview

toki-sync is a multi-device token usage sync server for the [toki](https://github.com/korjwl1/toki) ecosystem. It solves the problem of fragmented usage data: developers using AI tools across multiple machines (macOS desktop, Linux server, CI runners) have no unified view of their token consumption.

toki-sync collects delta events from toki daemons over persistent TCP connections, stores them in VictoriaMetrics as time-series data, and serves PromQL queries through an authenticated HTTP proxy. A single server instance handles personal use through enterprise teams, with SQLite for small deployments and PostgreSQL for scale.

The server is stateless by design. Auth and device metadata live in the database, time-series data lives in VictoriaMetrics, and the server itself is a protocol translator and auth gateway. This makes horizontal scaling straightforward: put N instances behind a load balancer.

## Architecture

```mermaid
graph TD
    subgraph Clients["toki Daemons (per device)"]
        D1["Device A<br/>sync thread → TCP+TLS"]
        D2["Device B<br/>sync thread → TCP+TLS"]
        D3["Device C<br/>sync thread → TCP+TLS"]
    end

    subgraph Server["toki-sync Server (Rust, tokio async)"]
        subgraph TCP["TCP :9090 — Sync Protocol"]
            Accept["accept loop<br/>Semaphore(500)"]
            Handler["per-connection handler<br/>AUTH → GET_LAST_TS → SYNC_BATCH loop"]
        end

        subgraph HTTP["HTTP :9091 — axum"]
            Auth["POST /login<br/>POST /register<br/>POST /token/refresh"]
            OIDC["GET /auth/oidc/authorize<br/>GET /auth/callback"]
            Proxy["GET /api/v1/query<br/>GET /api/v1/query_range<br/>JWT label injection"]
            Admin["GET /admin/users<br/>POST /admin/teams<br/>..."]
            Dashboard["GET /dashboard<br/>embedded web UI"]
        end

        subgraph Storage["Storage Layer"]
            DB["SQLite / PostgreSQL<br/>users, devices, cursors,<br/>refresh_tokens, teams"]
            WriteSem["VM Write Semaphore(10)<br/>global batch throttle"]
        end
    end

    subgraph External["External Services"]
        VM["VictoriaMetrics<br/>/api/v1/import/prometheus<br/>(internal network only)"]
        Caddy["Caddy / nginx<br/>TLS termination"]
    end

    subgraph Consumers["Query Consumers"]
        CLI["toki CLI<br/>--remote flag"]
        Monitor["Toki Monitor<br/>Server mode"]
        Web["Web Browser<br/>built-in dashboard"]
    end

    D1 & D2 & D3 -->|"TCP+TLS<br/>bincode frames"| Caddy
    Caddy -->|"plain TCP"| Accept
    Accept --> Handler
    Handler -->|"Prometheus text<br/>import batches"| WriteSem
    WriteSem --> VM

    CLI & Monitor & Web -->|"HTTPS"| Caddy
    Caddy -->|"HTTP"| Auth & Proxy & Dashboard
    Proxy -->|"PromQL + user label"| VM
```

### Thread/Task Model

The server is fully async on tokio. There are no blocking threads or `spawn_blocking` calls — bincode deserialization is fast enough to run on the async executor, and VM writes are HTTP I/O.

| Resource | Bound | Purpose |
|----------|-------|---------|
| TCP connection Semaphore | 500 | Prevents fd exhaustion from connection floods |
| VM write Semaphore | 10 | Limits concurrent Prometheus text import requests to VM |
| Read timeout (server) | 120s | Drops idle connections that miss PING/PONG |
| Read timeout (client) | 90s | Detects dead server before server timeout fires |

## Sync Protocol

### Why TCP + bincode

Both endpoints are our own Rust binaries. HTTP and gRPC add overhead that provides no benefit when there is no browser, no third-party client, and no need for human-readable wire format:

| | TCP + bincode | HTTP/JSON | gRPC/protobuf |
|---|---|---|---|
| **Framing** | 8-byte header (type + length) | HTTP headers (~200-500 bytes) | HTTP/2 framing + protobuf |
| **Serialization** | Zero-copy bincode (~ns) | JSON parse (~us) | Protobuf decode (~ns) |
| **Connection** | Persistent, bidirectional | Request/response per batch | Persistent, but HTTP/2 overhead |
| **Flow control** | ACK per batch (natural) | Need custom polling | gRPC streaming (complex) |
| **Dependencies** | bincode (tiny) | hyper + serde_json | tonic + prost + protoc toolchain |

The protocol is intentionally simple: a persistent connection with typed frames. No multiplexing, no streams, no headers.

### Frame Format

```
┌──────────────────┬──────────────────┬─────────────────────────┐
│ Message Type     │ Payload Length   │ Payload                 │
│ u32 LE (4 bytes) │ u32 LE (4 bytes) │ N bytes (bincode/zstd)  │
└──────────────────┴──────────────────┴─────────────────────────┘
```

Payload length is capped at `MAX_PAYLOAD_SIZE` (16 MiB). Any frame exceeding this is rejected and the connection is closed immediately. After zstd decompression, output is capped at 64 MiB to guard against zstd bombs.

### Message Types

| Type | Code | Direction | Payload |
|------|------|-----------|---------|
| AUTH | 0x01 | client -> server | jwt, device_name, schema_version, provider, device_key, protocol_version |
| AUTH_OK | 0x02 | server -> client | device_id |
| AUTH_ERR | 0x03 | server -> client | reason, reset_required |
| GET_LAST_TS | 0x10 | client -> server | provider |
| LAST_TS | 0x11 | server -> client | last_ts_ms |
| SYNC_BATCH | 0x20 | client -> server | SyncBatchPayload |
| SYNC_ACK | 0x21 | server -> client | last_ts_ms |
| SYNC_ERR | 0x22 | server -> client | reason |
| SyncBatchZstd | 0x23 | client -> server | SyncBatchPayload (zstd compressed) |
| PING | 0x30 | client -> server | (empty) |
| PONG | 0x31 | server -> client | (empty) |

### Message Flow

```mermaid
sequenceDiagram
    participant C as Client (toki daemon)
    participant S as Server (toki-sync)
    participant VM as VictoriaMetrics

    C->>S: AUTH { jwt, device_key, provider, schema_version, protocol_version }
    S->>S: Verify JWT, register/lookup device
    S-->>C: AUTH_OK { device_id }

    C->>S: GET_LAST_TS { provider }
    S-->>C: LAST_TS { last_ts_ms }

    loop For each batch of up to 1,000 events
        C->>S: SYNC_BATCH { items, dict, provider }
        S->>VM: POST /api/v1/import/prometheus (text format)
        VM-->>S: 204 No Content
        S->>S: Update cursor (device_id, provider) = last_ts_ms
        S-->>C: SYNC_ACK { last_ts_ms }
    end

    Note over C,S: Idle — no network traffic

    loop Keepalive (60s interval)
        C->>S: PING
        S-->>C: PONG
    end

    Note over C,S: FlushNotify wakes sync thread

    C->>S: GET_LAST_TS { provider }
    S-->>C: LAST_TS { last_ts_ms }
    C->>S: SYNC_BATCH { new delta }
    S-->>C: SYNC_ACK { last_ts_ms }
```

### Why ACK-Based Flow Control

Each SYNC_BATCH must be acknowledged before the next is sent. This provides natural backpressure:

- If VM is slow, the server delays ACK, and the client automatically waits
- 100 devices x ~100KB per in-flight batch = ~10MB total server memory. No OOM risk
- No rate limit messages, no token bucket, no configuration needed
- The client's local fjall DB acts as a persistent queue: if the server is down, events accumulate locally and sync on reconnect

### Why Zstd at 100+ Items

Compression is applied only to batches with 100 or more items:

| Batch size | Uncompressed | Zstd compressed | Ratio |
|------------|-------------|-----------------|-------|
| 10 items | ~2 KB | ~1.8 KB | 1.1x (not worth the CPU) |
| 100 items | ~20 KB | ~5 KB | 4x |
| 1,000 items | ~200 KB | ~30 KB | 6-7x |

Initial bulk sync (full history) benefits significantly. Steady-state incremental sync (1-10 events) skips compression entirely. The message type field (SYNC_BATCH vs SyncBatchZstd) tells the server whether to decompress.

### Dedup Strategy

VictoriaMetrics deduplicates by `(timestamp_ms, label_set)`. The label set is `{device, model, provider, session, project, user}`. There is no `msg_id` in the labels.

Why this works:
- Two events from the same session at the same millisecond with the same model/project is physically impossible (API response time is hundreds of ms)
- ACK-lost retransmission sends identical `(timestamp, labels, value)` tuples, which VM resolves via last-write-wins with identical values
- Adding `msg_id` as a label would create one unique time series per event, causing cardinality explosion

No server-side msg_id tracking is needed. The `last_ts` cursor alone is sufficient.

### SyncBatch Structure

```rust
struct SyncBatchPayload {
    items: Vec<SyncItem>,
    dict: HashMap<u32, String>,  // dict ID -> decoded string
    provider: String,
}

struct SyncItem {
    ts_ms: i64,
    event: StoredEvent,  // dict-ID encoded
}
```

The client sends dict IDs (compact u32 references) along with a dict map that resolves them to strings. The server uses this map to decode IDs into human-readable labels before writing to VM.

**Why dict IDs + dict map instead of decoded strings**: toki's local DB stores events with dictionary-compressed fields (model, session_id, project). Sending the compact representation with a per-batch mapping avoids redundant string allocation on the client and reduces wire size — the dict map contains each unique string once, while items reference them by ID.

## Cursor Management

### Composite Key: (device_id, provider)

The server maintains one cursor per `(device_id, provider)` combination. toki uses separate DB files per provider (`claude_code.fjall`, `codex.fjall`), and each has independent event timelines.

```
device "macbook" x "claude_code" -> last_ts: 1743000000000
device "macbook" x "codex"       -> last_ts: 1742900000000
device "linux"   x "claude_code" -> last_ts: 1743100000000
```

Adding a new provider starts its cursor at 0, triggering a full sync for that provider without affecting others.

### Server Cursor as Single Source of Truth

The client never persists its own cursor. On every connection (including reconnects), the client asks the server for `LAST_TS` and computes the delta from its local DB. This eliminates an entire class of consistency bugs:

- No split-brain between client and server cursor
- No cursor file corruption or staleness after crash
- Reconnection after any failure reduces to: `GET_LAST_TS` -> query local DB for events after that timestamp -> send batches

### Write Ordering: VM Write Before Cursor Update

The server MUST execute in this order:

```
1. Write batch to VictoriaMetrics
2. Update cursor in SQLite/PG
3. Send SYNC_ACK to client
```

Reversing steps 1 and 2 (cursor update before VM write) causes **permanent data loss**: the cursor advances past events that were never written to VM. On reconnect, the client sees the advanced cursor and skips those events.

### Reconnection Scenarios

| Scenario | Server State | On Reconnect |
|----------|-------------|--------------|
| Batch sent, connection drops before VM write | VM: no data, cursor: unchanged | GET_LAST_TS -> full retransmission |
| VM write completes, connection drops before ACK | VM: has data, cursor: updated | GET_LAST_TS -> cursor advanced, no retransmission needed |
| VM write completes, connection drops before cursor update | VM: has data, cursor: old | GET_LAST_TS -> retransmission, VM dedup handles duplicates |
| Server down for hours/days | fjall DB accumulates locally | Reconnect -> GET_LAST_TS -> send accumulated delta |

All four scenarios result in zero data loss. The worst case is redundant retransmission, which VM dedup resolves silently.

## Authentication & Security

### JWT Access/Refresh with Rotation

```mermaid
sequenceDiagram
    participant C as Client
    participant S as Server
    participant DB as Database

    C->>S: POST /login { username, password }
    S->>DB: Verify bcrypt hash
    S->>DB: Store refresh_token (hashed)
    S-->>C: { access_token (1h), refresh_token (90d) }

    Note over C: access_token expires

    C->>S: POST /token/refresh { refresh_token }
    S->>DB: Verify + delete old refresh_token
    S->>DB: Store new refresh_token (hashed)
    S-->>C: { new access_token, new refresh_token }
```

**Why rotation instead of long-lived tokens**: A stolen refresh token can be used exactly once. On use, the old token is invalidated and a new one is issued. If the legitimate client and attacker both try to use the same refresh token, the second attempt fails, alerting to the compromise. Long-lived tokens have no such detection mechanism.

**Why password change revokes all refresh tokens**: When a user changes their password (via `/me/password` or `/admin/users/:id/password`), all existing refresh tokens for that user are deleted. This ensures that a compromised session cannot persist after a password reset.

### Brute Force Guard

Login attempts are tracked by `(IP, username)` composite key:

| Parameter | Default | Config Key |
|-----------|---------|------------|
| Max attempts | 5 | `brute_force_max_attempts` |
| Window | 300s (5 min) | `brute_force_window_secs` |
| Lockout | 900s (15 min) | `brute_force_lockout_secs` |

The guard uses a sweep-based cleanup: expired entries are removed periodically rather than on every request. A hard cap on total tracked entries prevents memory exhaustion from distributed attacks.

### PromQL Label Injection

VictoriaMetrics runs on an internal network with no authentication. All external queries pass through the toki-sync HTTP proxy, which:

1. Validates the JWT from the request
2. Extracts the `user` claim
3. Injects `{user="<username>"}` into the PromQL expression
4. Forwards the modified query to VM

**Why not just VM auth**: VM's built-in auth is binary (full access or no access). It cannot enforce per-user data isolation. The proxy layer adds row-level security by rewriting queries.

**Injection defense**: Label values are escaped (`\` -> `\\`, `"` -> `\"`) before insertion into the query. A naive string concatenation like `user="alice"} or {user="` would bypass isolation. The proxy either parses the PromQL AST and adds the matcher programmatically, or applies strict escaping before URL-encoding.

### OIDC Flow

```
1. GET /auth/oidc/authorize
   -> Redirect to OIDC provider (Google, GitHub, Okta)
   -> Include state + nonce parameters

2. User authenticates at OIDC provider

3. GET /auth/callback?code=...
   -> Exchange authorization code for tokens at provider's token_endpoint
   -> Verify id_token signature using JWKS from provider's jwks_uri
   -> Extract sub, email, name claims
   -> Auto-create user if first login (email as username)
   -> Issue toki-sync JWT (access + refresh)
```

OIDC configuration is discovered automatically from the `oidc_issuer` URL via `/.well-known/openid-configuration`.

## Database Design

### Why Dual Backend

| | SQLite | PostgreSQL |
|---|---|---|
| **Setup** | Zero-config, single file | Separate service, connection string |
| **Best for** | 1-50 devices, personal/small team | 50+ devices, enterprise |
| **Concurrency** | WAL mode (concurrent reads, serialized writes) | Full MVCC |
| **Ops** | Backup = copy file | pg_dump, replication |
| **Migration** | Included in server binary | Included in server binary |

SQLite uses WAL mode (`PRAGMA journal_mode=WAL`) to handle concurrent reads from HTTP handlers and writes from sync handlers without lock contention.

### Repository Trait Abstraction

```rust
#[async_trait]
trait UserRepository {
    async fn find_by_username(&self, username: &str) -> Result<Option<User>>;
    async fn create(&self, user: &NewUser) -> Result<User>;
    // ...
}
```

SQLite and PostgreSQL implement the same trait. The backend is selected at startup based on `config.toml`:

```toml
[storage]
backend = "sqlite"      # or "postgres"
sqlite_path = "./data/toki_sync.db"
# postgres_url = "postgresql://user:pass@host:5432/toki_sync"
```

Same behavior, same tests, different driver. No application code changes when switching backends.

### Schema

```
users
  id          TEXT PRIMARY KEY
  username    TEXT UNIQUE NOT NULL
  password    TEXT          -- bcrypt hash (NULL for OIDC-only users)
  role        TEXT NOT NULL -- "admin" | "user"
  oidc_sub    TEXT          -- OIDC subject identifier
  created_at  INTEGER NOT NULL

devices
  id          TEXT PRIMARY KEY
  user_id     TEXT NOT NULL  -> users.id
  device_key  TEXT UNIQUE NOT NULL
  device_name TEXT NOT NULL
  token_columns TEXT          -- provider-specific token column schema (JSON)
  created_at  INTEGER NOT NULL

device_codes
  device_code TEXT PRIMARY KEY
  user_code   TEXT UNIQUE NOT NULL
  device_name TEXT NOT NULL
  user_id     TEXT            -- set when user verifies
  expires_at  INTEGER NOT NULL
  created_at  INTEGER NOT NULL

cursors
  device_id   TEXT NOT NULL  -> devices.id
  provider    TEXT NOT NULL
  last_ts_ms  INTEGER NOT NULL
  PRIMARY KEY (device_id, provider)

refresh_tokens
  id          TEXT PRIMARY KEY
  user_id     TEXT NOT NULL  -> users.id
  token_hash  TEXT NOT NULL  -- bcrypt hash of refresh token
  expires_at  INTEGER NOT NULL
  created_at  INTEGER NOT NULL

teams
  id          TEXT PRIMARY KEY
  name        TEXT UNIQUE NOT NULL
  created_at  INTEGER NOT NULL

team_members
  team_id     TEXT NOT NULL  -> teams.id
  user_id     TEXT NOT NULL  -> users.id
  PRIMARY KEY (team_id, user_id)
```

### Migration Strategy

Migrations use `ALTER TABLE` for SQLite and `IF NOT EXISTS` / `ADD COLUMN IF NOT EXISTS` for PostgreSQL. Schema version is tracked in a `meta` table. On startup, the server applies any pending migrations sequentially.

Schema mismatch between client and server (detected via `schema_version` in AUTH) triggers a clean re-sync: the server deletes the device's VM data and resets its cursor, and the client performs a full re-upload.

## VictoriaMetrics Integration

### Why VictoriaMetrics

| Requirement | VM Capability |
|-------------|---------------|
| PromQL-native queries | Full PromQL + MetricsQL extensions |
| Low resource usage | ~50 MB RAM for toki-scale workloads |
| Time-series at scale | Millions of series, hundreds of thousands of samples/sec |
| Built-in dedup | `dedup.minScrapeInterval` for ACK-lost retransmission |
| No external dependencies | Single binary, no cluster needed for Phase 1 |

Expected data volume for heavy personal use: ~207 events/min peak (3-4/sec). Even 16x this load (~3,300/min) is trivial for VM. Expected max cardinality: ~5,600 time series (session x model combinations).

### Prometheus Text Format

The server writes to VM using the Prometheus text exposition format via `POST /api/v1/import/prometheus`:

```
toki_input_tokens{device="macbook",model="claude-opus-4-6",provider="claude_code",session="sess-abc",project="myapp",user="alice"} 1500 1743000000000
toki_output_tokens{device="macbook",model="claude-opus-4-6",provider="claude_code",session="sess-abc",project="myapp",user="alice"} 800 1743000000000
toki_cache_creation_tokens{device="macbook",model="claude-opus-4-6",provider="claude_code",session="sess-abc",project="myapp",user="alice"} 200 1743000000000
toki_cache_read_tokens{device="macbook",model="claude-opus-4-6",provider="claude_code",session="sess-abc",project="myapp",user="alice"} 100 1743000000000
```

**Why Prometheus text format instead of remote_write protobuf**: VM supports both, but text format requires no protobuf dependency, no `.proto` compilation step, and is trivially debuggable with `curl`. The performance difference is negligible at toki's data volume.

### Label Structure

| Label | Source | Purpose |
|-------|--------|---------|
| `device` | AUTH device_name | Filter by machine |
| `model` | SyncItem (dict decoded) | Filter/group by AI model |
| `provider` | SyncBatchPayload.provider | Filter by tool (claude_code, codex) |
| `session` | SyncItem (dict decoded) | Drill down to individual sessions |
| `project` | SyncItem (dict decoded) | Group by codebase/project |
| `user` | JWT claim | Per-user data isolation |

Every dimension is PromQL-filterable. The `user` label is injected server-side, never sent by the client.

### Direct Text Generation

The server converts SyncBatchPayload directly to Prometheus text lines without an intermediate `MetricPoint` struct. Each SyncItem produces 4 text lines (one per token type). This avoids allocating and populating an intermediate representation that would immediately be serialized to text — a measurable saving when processing batches of 1,000 events (4,000 metric lines).

## Overload Protection

| Guard | Limit | Protects Against |
|-------|-------|------------------|
| TCP Semaphore | 500 connections | Connection flood / fd exhaustion |
| VM Write Semaphore | 10 concurrent | VM overload from simultaneous batch writes |
| Server read timeout | 120s | Dead/hung clients holding connections |
| Client read timeout | 90s | Detecting dead server before server-side timeout |
| MAX_PAYLOAD_SIZE | 16 MiB | Malicious clients sending oversized frames |
| Zstd decompression limit | 64 MiB | Zstd bomb (small compressed payload, huge output) |
| VM response limit | 32 MiB | Runaway PromQL queries returning excessive data |
| PING/PONG interval | 60s | NAT/firewall idle connection timeout |

**Why these specific numbers**:
- **500 connections**: Each connection is lightweight (one tokio task). 500 is well within fd limits and covers the largest expected deployment.
- **10 VM writes**: VM's `/api/v1/import/prometheus` endpoint handles requests sequentially internally. More than 10 concurrent requests just queue inside VM. Limiting server-side prevents unbounded task spawning.
- **120s/90s timeouts**: Client timeout must be shorter than server timeout so the client detects failure first and initiates reconnection, rather than the server dropping a connection the client still considers alive.
- **16 MiB payload**: A 1,000-event batch is ~200 KB uncompressed. 16 MiB allows for pathologically large batches while preventing multi-GB allocation attacks.
- **64 MiB decompression**: 4x the payload limit. A legitimate zstd-compressed 16 MiB payload decompresses to at most a few MB. 64 MiB catches bombs while allowing generous headroom.

## Crash Resilience

| Failure | Recovery |
|---------|----------|
| **Server process crash** | Docker `restart: unless-stopped` restarts the container. Clients detect via PONG timeout and reconnect with exponential backoff |
| **TCP handler panic** | Caught by tokio task. Logged, connection closed, accept loop continues serving other clients |
| **Accept loop error** | 1-second backoff, then resume accepting. Does not crash the server |
| **Client sync thread panic** | `catch_unwind` catches the panic. Auto-respawn after backoff |
| **Client worker thread panic** | Process exit. The system supervisor (launchd/systemd) restarts the daemon |
| **VM temporarily unavailable** | SYNC_ERR returned to client. Client disconnects and retries with backoff. No data loss (events remain in local fjall DB) |
| **SQLite/PG connection failure** | Auth and cursor operations fail. Existing sync connections continue until next DB-dependent operation |
| **Network partition** | PING/PONG timeout detection (max 60s). Reconnect with exponential backoff (cap: 5 min) |

The fjall DB on the client side is the ultimate safety net: events are never deleted from the local DB based on sync status. Even if the server loses all data, a client reconnect triggers a full re-sync from local history.

## Design Decisions & Tradeoffs

### Dict IDs + Dict Map vs Decoded Strings

| | Dict IDs + map | Pre-decoded strings |
|---|---|---|
| **Wire size** | Compact: u32 per field + one string per unique value | Full string per event field |
| **Client CPU** | Read dict from DB (already cached in memory) | Reverse-lookup every field for every event |
| **Server CPU** | One HashMap lookup per unique ID per batch | Zero |
| **Winner** | Bandwidth and client CPU (both sides are fast) | |

The dict map is per-batch, containing only the IDs used in that batch. A typical batch of 1,000 events might reference 3-5 models, 20-50 sessions, and 10-30 projects, producing a dict map of ~80 entries vs 1,000 x 4 = 4,000 string fields.

### SYNC_ACK last_ts_ms vs Count

SYNC_ACK echoes the server's cursor position (`last_ts_ms`) rather than a count of accepted events. This follows the Kafka/PostgreSQL cursor pattern:

- The client knows exactly where the server's cursor is
- No ambiguity about which events were accepted
- On reconnect, `GET_LAST_TS` returns this same value, creating a seamless resume point
- A count-based ACK would require the client to maintain its own cursor and reconcile with the server

### FlushNotify: Condvar vs Trace Client vs Polling

The sync thread needs to know when new events are committed to the local DB. Three options were evaluated:

| | Trace client reuse | DB polling | FlushNotify Condvar (adopted) |
|---|---|---|---|
| **Idle overhead** | BroadcastSink always active (atomic check per emit) | GET_LAST_TS every N seconds | **None** |
| **Latency** | Broadcast fires before DB commit (up to 1s race) | <= N seconds | **<= 1s (flush interval)** |
| **Complexity** | High (race conditions) | Low | **Low** |
| **Consistency** | DB commit not guaranteed at broadcast time | After DB commit | **After DB commit** |
| **Spurious wakeup** | Up to 64 per batch (one per emit_event) | None | **None (dirty flag coalescing)** |
| **Extra threads** | 2 (receiver + writer) | 0 | **0** |

Trace client reuse was structurally ruled out: `emit_event()` fires before `db_tx.send(WriteEvent)`, so the broadcast occurs before the DB commit. The sync thread would wake up and find no data in the DB. FlushNotify uses a `Condvar` + `AtomicBool` dirty flag that the DB writer sets after `flush_pending()` completes, guaranteeing post-commit notification with natural coalescing.

### Timer-Based Wake Detection vs OS-Native

Sleep/wake detection for reconnecting dead TCP connections:

| | Timer-based (current) | OS-native |
|---|---|---|
| **Detection latency** | Up to 60s (PING/PONG timeout) | Immediate |
| **Implementation** | Zero platform code | macOS: NSWorkspaceDidWakeNotification, Linux: systemd logind D-Bus |
| **Reliability** | Works everywhere | Requires platform-specific code paths |

The current implementation detects wake via PING/PONG timeout. A 60-second delay after wake is acceptable for a dashboard/monitoring use case. OS-native wake notifications are a planned future improvement.

### Device Code Authentication

The CLI uses the device code flow for authentication instead of passing credentials on the command line:

1. CLI calls `POST /auth/device/code` with the device name
2. Server returns a `user_code` and `verification_uri`
3. CLI opens the browser to the verification page and displays the user code
4. User logs in via the browser and enters the code
5. CLI polls `POST /auth/device/token` until the user completes authorization
6. Server returns JWT tokens upon successful verification

This eliminates the need for `--username` and `--password` flags, improving security by never exposing credentials in shell history or process listings.

For headless environments (no browser), the CLI prints the verification URL and user code for the user to enter manually on another device.

### Always-On Sync Thread with SyncToggle

The sync thread is spawned at daemon startup if sync is configured. Settings hot-reload allows enabling/disabling sync without daemon restart. This avoids:

- Runtime thread management complexity
- Race conditions between "sync just enabled" and "sync thread not yet ready"
- Configuration reload edge cases

When sync is disabled, the `flush_notify` field is `None`, and no sync-related code executes. Zero overhead.

### Registration Modes

The server supports three registration modes via `registration_mode`:

| Mode | Behavior |
|------|----------|
| `"open"` | Anyone can register via `POST /register` |
| `"approval"` | Registration creates a pending account; admin must approve via `/admin/pending/:id/approve` |
| `"closed"` | Only admins can create users via `/admin/users` |

### Additional Server Configuration

| Setting | Description |
|---------|-------------|
| `trust_proxy` | When `true`, the server reads client IP from `X-Forwarded-For` / `X-Real-IP` headers for brute force tracking. Only enable behind a trusted reverse proxy |
| `max_query_scope` | Limits the maximum time range for PromQL queries (e.g., `"365d"`). Prevents expensive queries spanning too much data |
| `max_concurrent_writes` | Limits parallel VictoriaMetrics batch writes (default: 10) |

## Scaling Guide

| Scale | Devices | Infra | Database | VictoriaMetrics | Notes |
|-------|---------|-------|----------|-----------------|-------|
| Personal | 1-10 | t2.micro (1 GB) | SQLite | Single node (~50 MB RAM) | Everything fits comfortably |
| Small team | 10-50 | t3.small (2 GB) | SQLite | Single node | SQLite WAL handles this concurrency fine |
| Medium team | 50-200 | t3.medium+ (4+ GB) | Consider PG | Single node + backup | PG recommended for concurrent HTTP + sync load |
| Enterprise | 200+ | Multiple instances | PostgreSQL (RDS) | VM cluster (vminsert/vmselect/vmstorage) | Stateless servers behind L4 LB + sticky sessions |

**Data volume reference**: A heavy personal user generates ~207 events/min at peak. 200 such users = ~41,400 events/min = ~690/sec. VM handles millions of samples/sec.

**Storage growth**: Each event produces 4 time series samples (~200 bytes in VM). 200 users x 1,000 events/day x 200 bytes = ~40 MB/day. With VM's compression, actual disk usage is significantly lower.
