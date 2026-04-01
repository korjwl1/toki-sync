# toki-sync Architecture & Design

## Overview

toki-sync는 [toki](https://github.com/korjwl1/toki) 생태계를 위한 멀티 디바이스 토큰 사용량 동기화 서버입니다. 여러 기기(macOS 데스크톱, Linux 서버, CI 러너 등)에서 AI 도구를 사용할 때 사용량 데이터가 분산되는 문제를 해결합니다.

toki-sync는 toki 데몬으로부터 persistent TCP 연결을 통해 델타 이벤트를 수집하고, EventStore(기본: Fjall 내장, 선택: ClickHouse)에 저장하며, 웹 대시보드와 선택적 PromQL 쿼리를 인증된 HTTP 프록시를 통해 제공합니다. 단일 서버 인스턴스로 개인 사용부터 엔터프라이즈 팀까지 처리하며, 소규모에는 SQLite를, 대규모에는 PostgreSQL을 사용합니다.

서버는 의도적으로 stateless로 설계되었습니다. 인증과 디바이스 메타데이터는 데이터베이스에, 이벤트 데이터는 EventStore(Fjall 또는 ClickHouse)에 저장되며, 서버 자체는 프로토콜 변환기이자 인증 게이트웨이 역할만 합니다. 따라서 수평 확장이 단순합니다: 로드밸런서 뒤에 N개 인스턴스를 배치하면 됩니다.

## Architecture

```mermaid
graph TD
    subgraph Clients["toki Daemon (디바이스별)"]
        D1["디바이스 A<br/>sync thread → TCP+TLS"]
        D2["디바이스 B<br/>sync thread → TCP+TLS"]
        D3["디바이스 C<br/>sync thread → TCP+TLS"]
    end

    subgraph Server["toki-sync Server (Rust, tokio async)"]
        subgraph TCP["TCP :9090 — Sync 프로토콜"]
            Accept["accept loop<br/>Semaphore(500)"]
            Handler["연결별 handler<br/>AUTH → GET_LAST_TS → SYNC_BATCH 루프"]
        end

        subgraph HTTP["HTTP :9091 — axum"]
            Auth["POST /login<br/>POST /register<br/>POST /token/refresh"]
            OIDC["GET /auth/oidc/authorize<br/>GET /auth/callback"]
            Proxy["GET /api/v1/query<br/>GET /api/v1/query_range<br/>JWT label injection"]
            Admin["GET /admin/users<br/>POST /admin/teams<br/>..."]
            Dashboard["GET /dashboard<br/>내장 웹 UI"]
        end

        subgraph Storage["Storage Layer"]
            DB["SQLite / PostgreSQL<br/>users, devices, cursors,<br/>refresh_tokens, teams"]
            ES["EventStore<br/>Fjall (내장) 또는 ClickHouse"]
            WriteSem["Write Semaphore(10)<br/>전역 배치 쓰기 제한"]
        end
    end

    subgraph External["외부 서비스"]
        Caddy["Caddy / nginx<br/>TLS termination"]
        VM["VictoriaMetrics (선택)<br/>PromQL 프록시 대상"]
    end

    subgraph Consumers["쿼리 소비자"]
        CLI["toki CLI<br/>--remote 플래그"]
        Monitor["Toki Monitor<br/>Server 모드"]
        Web["웹 브라우저<br/>내장 대시보드"]
    end

    D1 & D2 & D3 -->|"TCP+TLS<br/>bincode 프레임"| Caddy
    Caddy -->|"plain TCP"| Accept
    Accept --> Handler
    Handler -->|"이벤트 배치"| WriteSem
    WriteSem --> ES

    CLI & Monitor & Web -->|"HTTPS"| Caddy
    Caddy -->|"HTTP"| Auth & Proxy & Dashboard
    Proxy -.->|"PromQL + user label<br/>(선택)"| VM
```

### 스레드/태스크 모델

서버는 tokio 위에서 완전히 async로 동작합니다. 블로킹 스레드나 `spawn_blocking` 호출이 없습니다 — bincode 역직렬화는 async executor에서 실행할 만큼 빠르며, EventStore 쓰기는 I/O 바운드입니다.

| 리소스 | 제한 | 용도 |
|--------|------|------|
| TCP 연결 Semaphore | 500 | 연결 폭주 시 fd 고갈 방지 |
| Write Semaphore | 10 | EventStore에 대한 동시 배치 쓰기 제한 |
| 서버 읽기 타임아웃 | 120초 | PING/PONG을 놓친 유휴 연결 드롭 |
| 클라이언트 읽기 타임아웃 | 90초 | 서버 타임아웃 이전에 dead 서버 감지 |

## Sync Protocol

### TCP + bincode를 선택한 이유

양쪽 모두 우리의 Rust 바이너리입니다. 브라우저도 없고, 서드파티 클라이언트도 없고, 사람이 읽을 수 있는 와이어 포맷이 필요하지 않으므로 HTTP와 gRPC는 불필요한 오버헤드입니다:

| | TCP + bincode | HTTP/JSON | gRPC/protobuf |
|---|---|---|---|
| **프레이밍** | 8바이트 헤더 (타입 + 길이) | HTTP 헤더 (~200-500바이트) | HTTP/2 프레이밍 + protobuf |
| **직렬화** | 제로카피 bincode (~ns) | JSON 파싱 (~us) | Protobuf 디코딩 (~ns) |
| **연결** | Persistent, 양방향 | 배치마다 요청/응답 | Persistent이나 HTTP/2 오버헤드 |
| **흐름 제어** | 배치당 ACK (자연스러움) | 커스텀 폴링 필요 | gRPC 스트리밍 (복잡) |
| **의존성** | bincode (소형) | hyper + serde_json | tonic + prost + protoc 툴체인 |

프로토콜은 의도적으로 단순합니다: 타입이 지정된 프레임을 사용하는 persistent 연결입니다. 멀티플렉싱, 스트림, 헤더가 없습니다.

### 프레임 포맷

```
┌──────────────────┬──────────────────┬─────────────────────────┐
│ Message Type     │ Payload Length   │ Payload                 │
│ u32 LE (4바이트) │ u32 LE (4바이트) │ N바이트 (bincode/zstd)  │
└──────────────────┴──────────────────┴─────────────────────────┘
```

Payload 길이는 `MAX_PAYLOAD_SIZE` (16 MiB)로 제한됩니다. 이를 초과하는 프레임은 거부되고 연결이 즉시 종료됩니다. zstd 압축 해제 후 출력은 64 MiB로 제한하여 zstd 폭탄을 방어합니다.

### 메시지 타입

| 타입 | 코드 | 방향 | 페이로드 |
|------|------|------|----------|
| AUTH | 0x01 | 클라이언트 -> 서버 | jwt, device_name, schema_version, provider, device_key, protocol_version |
| AUTH_OK | 0x02 | 서버 -> 클라이언트 | device_id |
| AUTH_ERR | 0x03 | 서버 -> 클라이언트 | reason, reset_required |
| GET_LAST_TS | 0x10 | 클라이언트 -> 서버 | provider |
| LAST_TS | 0x11 | 서버 -> 클라이언트 | last_ts_ms |
| SYNC_BATCH | 0x20 | 클라이언트 -> 서버 | SyncBatchPayload |
| SYNC_ACK | 0x21 | 서버 -> 클라이언트 | last_ts_ms |
| SYNC_ERR | 0x22 | 서버 -> 클라이언트 | reason |
| SyncBatchZstd | 0x23 | 클라이언트 -> 서버 | SyncBatchPayload (zstd 압축) |
| PING | 0x30 | 클라이언트 -> 서버 | (비어 있음) |
| PONG | 0x31 | 서버 -> 클라이언트 | (비어 있음) |

### 메시지 흐름

```mermaid
sequenceDiagram
    participant C as 클라이언트 (toki daemon)
    participant S as 서버 (toki-sync)
    participant ES as EventStore

    C->>S: AUTH { jwt, device_key, provider, schema_version, protocol_version }
    S->>S: JWT 검증, 디바이스 등록/조회
    S-->>C: AUTH_OK { device_id }

    C->>S: GET_LAST_TS { provider }
    S-->>C: LAST_TS { last_ts_ms }

    loop 최대 1,000건 배치마다
        C->>S: SYNC_BATCH { items, dict, provider }
        S->>ES: 이벤트 쓰기 (Fjall insert / ClickHouse INSERT)
        ES-->>S: OK (msg_id dedup으로 중복 무시)
        S->>S: 커서 업데이트 (device_id, provider) = last_ts_ms
        S-->>C: SYNC_ACK { last_ts_ms }
    end

    Note over C,S: 유휴 — 네트워크 트래픽 없음

    loop Keepalive (60초 간격)
        C->>S: PING
        S-->>C: PONG
    end

    Note over C,S: FlushNotify가 sync thread를 깨움

    C->>S: GET_LAST_TS { provider }
    S-->>C: LAST_TS { last_ts_ms }
    C->>S: SYNC_BATCH { 새로운 delta }
    S-->>C: SYNC_ACK { last_ts_ms }
```

### ACK 기반 흐름 제어를 선택한 이유

각 SYNC_BATCH는 다음 배치 전송 전에 ACK를 받아야 합니다. 이는 자연스러운 backpressure를 제공합니다:

- EventStore가 느리면 서버가 ACK를 지연시키고, 클라이언트는 자동으로 대기합니다
- 100개 디바이스 x ~100KB/배치 = ~10MB 총 서버 메모리. OOM 위험 없음
- 별도의 rate limit 메시지, 토큰 버킷, 설정이 필요 없음
- 클라이언트의 로컬 fjall DB가 persistent 큐 역할: 서버가 다운되면 이벤트가 로컬에 축적되고 재연결 시 동기화

### Zstd를 100건 이상에서만 적용하는 이유

압축은 100건 이상의 배치에서만 적용됩니다:

| 배치 크기 | 비압축 | Zstd 압축 | 비율 |
|-----------|--------|-----------|------|
| 10건 | ~2 KB | ~1.8 KB | 1.1x (CPU 대비 효과 없음) |
| 100건 | ~20 KB | ~5 KB | 4x |
| 1,000건 | ~200 KB | ~30 KB | 6-7x |

초기 벌크 동기화(전체 이력)에서 큰 효과를 얻습니다. 정상 상태 증분 동기화(1-10건)에서는 압축을 완전히 생략합니다. 메시지 타입 필드(SYNC_BATCH vs SyncBatchZstd)가 서버에 압축 해제 여부를 알려줍니다.

### Dedup 전략

EventStore는 클라이언트가 각 이벤트에 대해 생성하는 고유 식별자 `msg_id`를 사용하여 이벤트를 중복 제거합니다.

- **Fjall**: `msg_id`에 대한 `idx_msg` 보조 인덱스와 고유 제약 조건을 사용합니다. 중복 삽입은 조용히 무시됩니다.
- **ClickHouse**: `msg_id`를 키로 하는 `ReplacingMergeTree` 엔진을 사용합니다. 백그라운드 병합 시 중복이 축소됩니다.

msg_id 기반 dedup의 이유:
- ACK 소실 재전송은 동일한 `msg_id` 값의 동일한 이벤트를 전송하며, 조용히 중복 제거됩니다
- `last_ts` 커서가 기본 동기화 메커니즘을 제공하며, msg_id dedup은 엣지 케이스의 안전망입니다
- cardinality 폭발 없음 (시계열 label로 msg_id를 추가하는 것과 달리)

### SyncBatch 구조

```rust
struct SyncBatchPayload {
    items: Vec<SyncItem>,
    dict: HashMap<u32, String>,  // dict ID -> 디코딩된 문자열
    provider: String,
}

struct SyncItem {
    ts_ms: i64,
    event: StoredEvent,  // dict ID로 인코딩됨
}
```

클라이언트는 dict ID(compact u32 참조)와 함께 이를 문자열로 변환하는 dict map을 전송합니다. 서버는 이 map을 사용해 ID를 사람이 읽을 수 있는 필드로 디코딩한 후 EventStore에 기록합니다.

**dict ID + dict map을 디코딩된 문자열 대신 사용하는 이유**: toki의 로컬 DB는 딕셔너리 압축된 필드(model, session_id, project)로 이벤트를 저장합니다. compact 표현과 배치별 매핑을 함께 전송하면 클라이언트에서의 불필요한 문자열 할당을 피하고 와이어 크기를 줄입니다 — dict map은 각 고유 문자열을 한 번만 포함하고, item들은 ID로 참조합니다.

## Cursor Management

### 복합 키: (device_id, provider)

서버는 `(device_id, provider)` 조합마다 하나의 커서를 유지합니다. toki는 provider별로 별도 DB 파일(`claude_code.fjall`, `codex.fjall`)을 사용하며, 각각 독립적인 이벤트 타임라인을 가집니다.

```
device "macbook" x "claude_code" -> last_ts: 1743000000000
device "macbook" x "codex"       -> last_ts: 1742900000000
device "linux"   x "claude_code" -> last_ts: 1743100000000
```

새 provider를 추가하면 해당 조합의 커서가 0에서 시작하여 해당 provider만 전체 동기화가 트리거됩니다.

### 서버 커서가 유일한 진실 소스

클라이언트는 자체 커서를 영속화하지 않습니다. 재연결을 포함한 모든 연결에서 클라이언트는 서버에 `LAST_TS`를 요청하고 로컬 DB에서 delta를 계산합니다. 이는 일관성 버그의 전체 클래스를 제거합니다:

- 클라이언트와 서버 커서 간 split-brain 없음
- 크래시 후 커서 파일 손상이나 stale 상태 없음
- 어떤 장애 후 재연결이든 다음으로 축소: `GET_LAST_TS` -> 해당 타임스탬프 이후의 로컬 DB 이벤트 쿼리 -> 배치 전송

### 쓰기 순서: EventStore 쓰기가 커서 업데이트보다 먼저

서버는 반드시 다음 순서로 실행해야 합니다:

```
1. EventStore(Fjall / ClickHouse)에 배치 쓰기
2. SQLite/PG에 커서 업데이트
3. 클라이언트에 SYNC_ACK 전송
```

1단계와 2단계를 역전하면(EventStore 쓰기 전에 커서 업데이트) **영구적 데이터 손실**이 발생합니다: 기록되지 않은 이벤트를 지나 커서가 전진합니다. 재연결 시 클라이언트는 전진된 커서를 보고 해당 이벤트를 건너뜁니다.

### 재연결 시나리오

| 시나리오 | 서버 상태 | 재연결 시 동작 |
|----------|-----------|----------------|
| 배치 전송 후 EventStore 쓰기 전 연결 끊김 | EventStore: 데이터 없음, 커서: 변경 없음 | GET_LAST_TS -> 전체 재전송 |
| EventStore 쓰기 완료 후 ACK 전 연결 끊김 | EventStore: 데이터 있음, 커서: 업데이트됨 | GET_LAST_TS -> 커서 전진, 재전송 불필요 |
| EventStore 쓰기 완료 후 커서 업데이트 전 연결 끊김 | EventStore: 데이터 있음, 커서: 이전 위치 | GET_LAST_TS -> 재전송, msg_id dedup이 중복 처리 |
| 서버 수시간/수일 다운 | fjall DB에 로컬 축적 | 재연결 -> GET_LAST_TS -> 축적된 delta 전송 |

네 가지 시나리오 모두 데이터 손실이 없습니다. 최악의 경우는 중복 재전송이며, msg_id 기반 dedup이 이를 조용히 해결합니다.

## Authentication & Security

### JWT Access/Refresh with Rotation

```mermaid
sequenceDiagram
    participant C as 클라이언트
    participant S as 서버
    participant DB as 데이터베이스

    C->>S: POST /login { username, password }
    S->>DB: bcrypt 해시 검증
    S->>DB: refresh_token 저장 (해시됨)
    S-->>C: { access_token (1시간), refresh_token (90일) }

    Note over C: access_token 만료

    C->>S: POST /token/refresh { refresh_token }
    S->>DB: 기존 refresh_token 검증 + 삭제
    S->>DB: 새 refresh_token 저장 (해시됨)
    S-->>C: { 새 access_token, 새 refresh_token }
```

**로테이션을 선택한 이유 (장기 토큰 대신)**: 탈취된 refresh token은 정확히 한 번만 사용할 수 있습니다. 사용 시 기존 토큰은 무효화되고 새 토큰이 발급됩니다. 정당한 클라이언트와 공격자가 동일한 refresh token을 사용하려 하면 두 번째 시도가 실패하여 침해를 경고합니다. 장기 토큰에는 이러한 감지 메커니즘이 없습니다.

**비밀번호 변경 시 모든 refresh token을 폐기하는 이유**: 사용자가 비밀번호를 변경하면(`/me/password` 또는 `/admin/users/:id/password`) 해당 사용자의 모든 기존 refresh token이 삭제됩니다. 이는 비밀번호 재설정 후에도 침해된 세션이 지속되는 것을 방지합니다.

### Brute Force 방어

로그인 시도는 `(IP, username)` 복합 키로 추적됩니다:

| 파라미터 | 기본값 | 설정 키 |
|----------|--------|---------|
| 최대 시도 횟수 | 5 | `brute_force_max_attempts` |
| 윈도우 | 300초 (5분) | `brute_force_window_secs` |
| 잠금 | 900초 (15분) | `brute_force_lockout_secs` |

가드는 sweep 기반 정리를 사용합니다: 만료된 항목은 매 요청이 아닌 주기적으로 제거됩니다. 추적 항목의 하드 캡으로 분산 공격에 의한 메모리 고갈을 방지합니다.

### PromQL Label Injection (선택, VictoriaMetrics 필요)

`[backend].vm_url`로 VictoriaMetrics가 설정되면 서버는 PromQL 프록시를 제공합니다. VictoriaMetrics는 인증 없이 내부 네트워크에서 실행됩니다. 모든 외부 쿼리는 toki-sync HTTP 프록시를 통과하며, 프록시는:

1. 요청의 JWT를 검증합니다
2. `user` claim을 추출합니다
3. PromQL 표현식에 `{user="<username>"}`을 주입합니다
4. 수정된 쿼리를 VictoriaMetrics에 전달합니다

이것은 PromQL 기반 도구(toki CLI `--remote`, Toki Monitor 서버 모드)와의 하위 호환성을 위한 선택적 기능입니다. 핵심 동기화 기능은 VictoriaMetrics를 필요로 하지 않습니다.

**인젝션 방어**: label 값은 쿼리에 삽입하기 전에 이스케이프(`\` -> `\\`, `"` -> `\"`)됩니다. `user="alice"} or {user="`와 같은 순진한 문자열 연결은 격리를 우회합니다. 프록시는 PromQL AST를 파싱하여 프로그래밍 방식으로 matcher를 추가하거나, URL 인코딩 전에 엄격한 이스케이프를 적용합니다.

### OIDC 플로우

```
1. GET /auth/oidc/authorize
   -> OIDC 프로바이더(Google, GitHub, Okta)로 리다이렉트
   -> state + nonce 파라미터 포함

2. 사용자가 OIDC 프로바이더에서 인증

3. GET /auth/callback?code=...
   -> 프로바이더의 token_endpoint에서 authorization code 교환
   -> 프로바이더의 jwks_uri에서 가져온 JWKS로 id_token 서명 검증
   -> sub, email, name claim 추출
   -> 최초 로그인 시 사용자 자동 생성 (email을 username으로 사용)
   -> toki-sync JWT (access + refresh) 발급
```

OIDC 설정은 `oidc_issuer` URL에서 `/.well-known/openid-configuration`을 통해 자동으로 검색됩니다.

## Database Design

### 듀얼 백엔드를 선택한 이유

| | SQLite | PostgreSQL |
|---|---|---|
| **설정** | 설정 불필요, 단일 파일 | 별도 서비스, 연결 문자열 |
| **적합 대상** | 1-50 디바이스, 개인/소규모 팀 | 50+ 디바이스, 엔터프라이즈 |
| **동시성** | WAL 모드 (동시 읽기, 직렬화된 쓰기) | Full MVCC |
| **운영** | 백업 = 파일 복사 | pg_dump, 레플리케이션 |
| **마이그레이션** | 서버 바이너리에 포함 | 서버 바이너리에 포함 |

SQLite는 WAL 모드(`PRAGMA journal_mode=WAL`)를 사용하여 HTTP 핸들러의 동시 읽기와 sync 핸들러의 쓰기를 lock 경합 없이 처리합니다.

### Repository Trait 추상화

```rust
#[async_trait]
trait UserRepository {
    async fn find_by_username(&self, username: &str) -> Result<Option<User>>;
    async fn create(&self, user: &NewUser) -> Result<User>;
    // ...
}
```

SQLite와 PostgreSQL이 동일한 trait을 구현합니다. 백엔드는 `config.toml` 기반으로 시작 시 선택됩니다:

```toml
[storage]
backend = "sqlite"      # 또는 "postgres"
sqlite_path = "./data/toki_sync.db"
# postgres_url = "postgresql://user:pass@host:5432/toki_sync"
```

동일한 동작, 동일한 테스트, 다른 드라이버. 백엔드 전환 시 애플리케이션 코드 변경이 필요 없습니다.

### 스키마

```
users
  id          TEXT PRIMARY KEY
  username    TEXT UNIQUE NOT NULL
  password    TEXT          -- bcrypt 해시 (OIDC 전용 사용자는 NULL)
  role        TEXT NOT NULL -- "admin" | "user"
  oidc_sub    TEXT          -- OIDC subject 식별자
  created_at  INTEGER NOT NULL

devices
  id          TEXT PRIMARY KEY
  user_id     TEXT NOT NULL  -> users.id
  device_key  TEXT UNIQUE NOT NULL
  device_name TEXT NOT NULL
  created_at  INTEGER NOT NULL

cursors
  device_id   TEXT NOT NULL  -> devices.id
  provider    TEXT NOT NULL
  last_ts_ms  INTEGER NOT NULL
  PRIMARY KEY (device_id, provider)

refresh_tokens
  id          TEXT PRIMARY KEY
  user_id     TEXT NOT NULL  -> users.id
  token_hash  TEXT NOT NULL  -- refresh token의 bcrypt 해시
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

### 마이그레이션 전략

마이그레이션은 SQLite에서 `ALTER TABLE`을, PostgreSQL에서 `IF NOT EXISTS` / `ADD COLUMN IF NOT EXISTS`를 사용합니다. 스키마 버전은 `meta` 테이블에서 추적됩니다. 시작 시 서버는 보류 중인 마이그레이션을 순차적으로 적용합니다.

클라이언트와 서버 간 스키마 불일치(AUTH의 `schema_version`으로 감지)는 클린 재동기화를 트리거합니다: 서버가 해당 디바이스의 이벤트 데이터를 삭제하고 커서를 리셋하면, 클라이언트가 전체 재업로드를 수행합니다.

## EventStore Integration

### Fjall을 선택한 이유 (기본)

| 요구사항 | Fjall 역량 |
|----------|-----------|
| 외부 의존성 없음 | 내장 Rust 라이브러리, 별도 프로세스 불필요 |
| 낮은 리소스 사용량 | toki 규모 워크로드에서 최소한의 RAM 오버헤드 |
| 내장 dedup | ACK 소실 재전송을 위한 `msg_id`의 `idx_msg` 고유 인덱스 |
| 간단한 운영 | 데이터는 디스크의 디렉토리; 백업 = 디렉토리 복사 |

### ClickHouse를 선택한 이유 (선택)

| 요구사항 | ClickHouse 역량 |
|----------|----------------|
| 대규모 분석 | 집계 쿼리에 최적화된 컬럼 지향 저장소 |
| 높은 쓰기 처리량 | 초당 수백만 건의 삽입 처리 |
| 내장 dedup | `msg_id`를 키로 하는 `ReplacingMergeTree` 엔진으로 중복 축소 |
| SQL 쿼리 | 임시 분석을 위한 표준 SQL |

헤비 개인 사용자의 예상 데이터 볼륨: 피크 시 ~207건/분 (3-4건/초). 이 부하의 16배(~3,300건/분)조차 두 백엔드 모두 쉽게 처리합니다.

### 이벤트 필드

| 필드 | 출처 | 용도 |
|------|------|------|
| `msg_id` | 클라이언트 생성 UUID | 중복 제거 키 |
| `device` | AUTH device_name | 기기별 필터 |
| `model` | SyncItem (dict 디코딩됨) | AI 모델별 필터/그룹 |
| `provider` | SyncBatchPayload.provider | 도구별 필터 (claude_code, codex) |
| `session` | SyncItem (dict 디코딩됨) | 개별 세션으로 드릴다운 |
| `project` | SyncItem (dict 디코딩됨) | 코드베이스/프로젝트별 그룹 |
| `user` | JWT claim | 사용자별 데이터 격리 |
| `ts_ms` | SyncItem 타임스탬프 | 이벤트 타임스탬프 |
| `input_tokens`, `output_tokens` 등 | SyncItem 토큰 수 | 사용량 메트릭 |

## Overload Protection

| 가드 | 제한 | 방어 대상 |
|------|------|-----------|
| TCP Semaphore | 500 연결 | 연결 폭주 / fd 고갈 |
| Write Semaphore | 10 동시 | 동시 배치 쓰기에 의한 EventStore 과부하 |
| 서버 읽기 타임아웃 | 120초 | 연결을 유지하는 dead/hung 클라이언트 |
| 클라이언트 읽기 타임아웃 | 90초 | 서버 측 타임아웃 전에 dead 서버 감지 |
| MAX_PAYLOAD_SIZE | 16 MiB | 악의적 클라이언트의 대형 프레임 전송 |
| Zstd 압축 해제 제한 | 64 MiB | Zstd 폭탄 (작은 압축, 거대한 출력) |
| 쿼리 응답 제한 | 32 MiB | 과도한 데이터를 반환하는 쿼리 |
| PING/PONG 간격 | 60초 | NAT/방화벽 유휴 연결 타임아웃 |

**이 구체적인 수치를 선택한 이유**:
- **500 연결**: 각 연결은 경량(tokio 태스크 하나). 500은 fd 제한 내에 충분히 있으며 예상되는 최대 배포를 커버합니다.
- **10 쓰기**: EventStore에 대한 동시 배치 쓰기를 제한합니다. 10개 이상의 동시 요청은 큐잉됩니다. 서버 측 제한으로 무제한 태스크 생성을 방지합니다.
- **120초/90초 타임아웃**: 클라이언트 타임아웃이 서버 타임아웃보다 짧아야 클라이언트가 먼저 장애를 감지하고 재연결을 시작합니다. 서버가 클라이언트가 아직 살아있다고 간주하는 연결을 드롭하는 것을 방지합니다.
- **16 MiB 페이로드**: 1,000건 배치는 비압축 ~200 KB입니다. 16 MiB는 비정상적으로 큰 배치를 허용하면서 다수 GB 할당 공격을 방지합니다.
- **64 MiB 압축 해제**: 페이로드 제한의 4배. 정상적인 zstd 압축 16 MiB 페이로드는 최대 수 MB로 압축 해제됩니다. 64 MiB는 폭탄을 잡으면서 충분한 여유를 허용합니다.

## Crash Resilience

| 장애 | 복구 |
|------|------|
| **서버 프로세스 크래시** | Docker `restart: unless-stopped`가 컨테이너를 재시작. 클라이언트는 PONG 타임아웃으로 감지하고 exponential backoff로 재연결 |
| **TCP 핸들러 panic** | tokio 태스크가 포착. 로그 기록, 연결 종료, accept 루프는 다른 클라이언트를 계속 서비스 |
| **Accept 루프 에러** | 1초 backoff 후 accept 재개. 서버 크래시 아님 |
| **클라이언트 sync thread panic** | `catch_unwind`가 panic 포착. backoff 후 자동 재생성 |
| **클라이언트 worker thread panic** | 프로세스 종료. 시스템 supervisor(launchd/systemd)가 데몬을 재시작 |
| **EventStore 일시적 불가** | 클라이언트에 SYNC_ERR 반환. 클라이언트가 연결 해제 후 backoff로 재시도. 데이터 손실 없음 (이벤트가 로컬 fjall DB에 남아 있음) |
| **SQLite/PG 연결 실패** | 인증과 커서 작업이 실패. 기존 sync 연결은 다음 DB 의존 작업까지 계속 |
| **네트워크 파티션** | PING/PONG 타임아웃 감지 (최대 60초). exponential backoff로 재연결 (캡: 5분) |

클라이언트 측 fjall DB가 궁극적인 안전망입니다: sync 상태에 기반하여 로컬 DB에서 이벤트가 삭제되지 않습니다. 서버가 모든 데이터를 잃어도 클라이언트 재연결이 로컬 이력에서 전체 재동기화를 트리거합니다.

## Design Decisions & Tradeoffs

### Dict ID + Dict Map vs 디코딩된 문자열

| | Dict ID + map | 사전 디코딩된 문자열 |
|---|---|---|
| **와이어 크기** | Compact: 필드당 u32 + 고유 값당 문자열 하나 | 이벤트 필드마다 전체 문자열 |
| **클라이언트 CPU** | DB에서 dict 읽기 (이미 메모리에 캐시됨) | 모든 이벤트의 모든 필드를 역변환 |
| **서버 CPU** | 배치당 고유 ID별 HashMap 조회 1회 | 없음 |
| **승자** | 대역폭과 클라이언트 CPU (양쪽 다 빠름) | |

dict map은 배치별이며 해당 배치에서 사용된 ID만 포함합니다. 1,000건의 일반적인 배치는 3-5개 모델, 20-50개 세션, 10-30개 프로젝트를 참조하여 ~80 항목의 dict map을 생성합니다. 반면 디코딩된 문자열은 1,000 x 4 = 4,000개의 문자열 필드를 전송합니다.

### SYNC_ACK last_ts_ms vs Count

SYNC_ACK는 수락된 이벤트 수가 아닌 서버의 커서 위치(`last_ts_ms`)를 echo합니다. Kafka/PostgreSQL 커서 패턴을 따릅니다:

- 클라이언트는 서버 커서가 정확히 어디인지 알 수 있습니다
- 어떤 이벤트가 수락되었는지 모호함이 없습니다
- 재연결 시 `GET_LAST_TS`가 동일한 값을 반환하여 매끄러운 재개 지점을 만듭니다
- count 기반 ACK는 클라이언트가 자체 커서를 유지하고 서버와 조정해야 합니다

### FlushNotify: Condvar vs Trace Client vs Polling

sync thread는 로컬 DB에 새 이벤트가 commit되었는지 알아야 합니다. 세 가지 옵션을 평가했습니다:

| | trace client 재사용 | DB 폴링 | FlushNotify Condvar (채택) |
|---|---|---|---|
| **유휴 오버헤드** | BroadcastSink 항상 활성 (emit마다 atomic 확인) | N초마다 GET_LAST_TS | **없음** |
| **지연** | broadcast가 DB commit 이전 (최대 1초 race) | <= N초 | **<= 1초 (flush interval)** |
| **복잡도** | 높음 (race condition) | 낮음 | **낮음** |
| **일관성** | broadcast 시점에 DB commit 보장 없음 | DB commit 이후 | **DB commit 이후** |
| **spurious wakeup** | 배치당 최대 64회 (emit_event당 1회) | 없음 | **없음 (dirty flag coalescing)** |
| **추가 스레드** | 2개 (receiver + writer) | 0 | **0** |

trace client 재사용은 구조적으로 배제되었습니다: `emit_event()`가 `db_tx.send(WriteEvent)` 이전에 실행되므로 broadcast가 DB commit 이전에 발생합니다. sync thread가 깨어나도 DB에 데이터가 없습니다. FlushNotify는 DB writer가 `flush_pending()` 완료 후 설정하는 `Condvar` + `AtomicBool` dirty flag를 사용하여 commit 후 알림과 자연스러운 coalescing을 보장합니다.

### 타이머 기반 Wake 감지 vs OS 네이티브

dead TCP 연결 재연결을 위한 수면/wake 감지:

| | 타이머 기반 (현재) | OS 네이티브 |
|---|---|---|
| **감지 지연** | 최대 60초 (PING/PONG 타임아웃) | 즉시 |
| **구현** | 플랫폼 코드 없음 | macOS: NSWorkspaceDidWakeNotification, Linux: systemd logind D-Bus |
| **신뢰성** | 모든 곳에서 동작 | 플랫폼별 코드 경로 필요 |

현재 구현은 PING/PONG 타임아웃으로 wake를 감지합니다. wake 후 60초 지연은 대시보드/모니터링 용도에서 수용 가능합니다. OS 네이티브 wake 알림은 향후 개선 사항으로 계획되어 있습니다.

### Always-On Sync Thread with SyncToggle

sync thread는 sync가 설정되어 있으면 데몬 시작 시 생성됩니다. hot-add 기능은 없습니다 (sync 활성화는 데몬 재시작이 필요). 이로써 다음을 피합니다:

- 런타임 스레드 관리 복잡성
- "sync 방금 활성화"와 "sync thread 아직 미준비" 간의 race condition
- 설정 리로드 edge case

sync가 비활성화되면 `flush_notify` 필드가 `None`이며, sync 관련 코드가 실행되지 않습니다. 오버헤드 제로입니다.

## Scaling Guide

| 규모 | 디바이스 | 인프라 | 데이터베이스 | EventStore | 비고 |
|------|----------|--------|-------------|------------|------|
| 개인 | 1-10 | t2.micro (1 GB) | SQLite | Fjall (내장) | 모든 것이 여유롭게 동작, 외부 의존성 없음 |
| 소규모 팀 | 10-50 | t3.small (2 GB) | SQLite | Fjall (내장) | SQLite WAL이 이 수준의 동시성을 처리 |
| 중규모 팀 | 50-200 | t3.medium+ (4+ GB) | PG 고려 | Fjall 또는 ClickHouse | 동시 HTTP + sync 부하에 PG 권장 |
| 엔터프라이즈 | 200+ | 다중 인스턴스 | PostgreSQL (RDS) | ClickHouse 클러스터 | Stateless 서버를 L4 LB + sticky session 뒤에 배치 |

**데이터 볼륨 참고**: 헤비 개인 사용자는 피크 시 ~207건/분을 생성합니다. 200명의 이런 사용자 = ~41,400건/분 = ~690건/초. Fjall과 ClickHouse 모두 이를 쉽게 처리합니다.

**스토리지 증가**: 각 이벤트는 ~200바이트입니다. 200 사용자 x 1,000건/일 x 200바이트 = ~40 MB/일. Fjall의 LSM 압축 또는 ClickHouse의 컬럼 압축으로 실제 디스크 사용량은 상당히 낮습니다.
