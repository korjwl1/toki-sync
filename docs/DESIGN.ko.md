# toki-sync 아키텍처와 설계

이 문서는 현재 브랜치 코드를 설명합니다. 계획 단계의 VictoriaMetrics, PromQL
프록시, 내장 사용량 대시보드, 다중 replica 기능을 구현된 것처럼 서술하지 않습니다.

## 구현된 토폴로지

```text
toki daemon들
    | 배포 내부 평문 TCP :9090
    | bincode frame, 선택적 zstd batch payload
    v
toki-sync process
    |-- 관계형 repository: SQLite(기본) 또는 PostgreSQL
    |-- 이벤트 repository: Fjall(기본) 또는 ClickHouse
    |-- HTTP :9091
        |-- 인증/device-code/OIDC
        |-- /api/v1/toki/query
        |-- 사용자/admin/team API
        +-- 선택적 toki_monitor 설정 채널

TLS terminator(배포 책임)
    |-- HTTPS -> :9091
    +-- TLS TCP -> :9090
```

프로세스 자체는 평문 TCP와 HTTP를 수신합니다. 번들 Caddy 프로필은 내부 CA로 TLS를
종단할 수 있고 외부 리버스 프록시는 공개 인증서를 제공할 수 있습니다. 현재
Caddyfile은 문서에 있던 DuckDNS DNS challenge를 구현하지 않습니다.

`/admin`은 사용자/디바이스/팀/설정 관리 콘솔입니다. 내장 토큰 사용량 차트 경로는
없습니다. 사용량 소비자는 쿼리 API 또는 Toki Monitor 같은 별도 앱을 사용합니다.

## 런타임 태스크와 동시성

- Tokio 태스크가 HTTP 연결을 수락합니다.
- 별도 listener 태스크가 TCP sync 연결을 수락합니다.
- TCP client마다 handler 태스크 하나를 사용합니다.
- semaphore가 500개를 넘는 연결을 거부합니다.
- 설정 가능한 semaphore(기본 10)가 동시 EventStore 배치 쓰기를 제한합니다.
- 구현상 필요한 Fjall, ClickHouse/ureq, bcrypt, pricing 작업은 blocking task로
  이동합니다.
- 종료 시 새 TCP 연결을 막고 최대 30초 기다린 뒤 남은 handler를 중단합니다.
- cleanup과 pricing refresh는 6시간마다 실행됩니다.

현재는 단일 프로세스 설계입니다. Fjall은 내장형이고 SQLite는 로컬 파일입니다.
ClickHouse window merge lock과 monitor/window rate limiter도 프로세스 로컬입니다.
같은 스토어에 여러 toki-sync replica를 실행하는 것은 안전하다고 문서화되지 않았고
분산 coordination 테스트도 없습니다.

## Sync protocol

frame 구조는 다음과 같습니다.

```text
[message type: u32 little endian]
[payload length: u32 little endian]
[bincode payload bytes]
```

형제 crate의 상수는 protocol version 1, event schema version 3, frame payload 최대
16 MiB입니다. 인증 전에는 첫 frame을 64 KiB로 추가 제한하고 10초 안에 받아야
합니다. 일반 연결 read timeout은 120초입니다.

구현된 메시지:

| 그룹 | 메시지 |
|---|---|
| 인증 | `AUTH`, `AUTH_OK`, `AUTH_ERR` |
| 커서 | `GET_LAST_TS`, `LAST_TS` |
| 이벤트 | `SYNC_BATCH`, `SYNC_BATCH_ZSTD`, `SYNC_ACK`, `SYNC_ERR` |
| 윈도우 | `SYNC_WINDOWS` (`sync_windows_v1` capability 필요) |
| keepalive | `PING`, `PONG` |

Bincode struct는 필드 순서에 민감합니다. 알 수 없는 message discriminant는 연결
오류이므로 선택 메시지를 보내기 전에 공개 `GET /api/v1/capabilities`를 확인해야
합니다.

### 이벤트 배치 흐름

1. 클라이언트가 JWT, stable device key, provider, protocol version, schema version으로
   인증합니다.
2. 서버가 해당 사용자의 device와 provider cursor를 찾거나 만듭니다.
3. 클라이언트가 `last_ts_ms`를 받고 그보다 이후의 로컬 이벤트를 올립니다.
4. 서버가 dictionary 참조와 token column을 검증하고 wire event를 해석합니다. decode
   후 batch는 최대 50,000개입니다.
5. EventStore 쓰기가 끝난 뒤 관계형 cursor를 전진시킵니다.
6. `SYNC_ACK`가 서버 cursor를 알립니다.

zstd frame은 최대 64 MiB로 압축 해제할 수 있습니다. window payload 최대는 1 MiB이고
최신 2,000개 window 항목까지만 고려합니다. window batch는 user/provider마다 1분에
한 번 수락하도록 rate limit합니다.

protocol version이 다르면 인증에 실패합니다. 이벤트 schema version이 다르면 해당
device의 원격 이벤트를 삭제하고 provider cursor를 reset한 뒤 전체 재동기화를
요구합니다.

### 커서와 중복 제거 invariant

관계형 cursor 키는 `(device_id, provider)`입니다. 이벤트 upsert의 멱등 키는
`(device_id, provider, msg_id)`입니다. Fjall은 그 키의 secondary dedup index를
사용하고 최신 timestamp가 이깁니다. 새 ClickHouse 테이블은
`ReplacingMergeTree(ts_ms)`와 `ORDER BY (device_id, provider, msg_id)`를 사용하며
쿼리는 `FINAL`을 사용합니다.

이는 transactional exactly-once가 아니라 멱등 replay입니다. EventStore 쓰기는
성공하고 cursor update가 실패할 수 있으며, 이후 client 재전송과 dedup으로 수렴합니다.

Fjall dedup index는 기본 30일인 `dedup_retention_secs` 이후 정리됩니다. 이 cleanup은
event record를 삭제하지 않습니다.

## 관계형 스토리지

`DatabaseRepo`는 사용자, device, cursor, refresh token, device code, 가입 대기,
team, 동적 설정, 활성 상태, monitor 설정에 대한 SQLite와 PostgreSQL 구현을 가집니다.

SQLite는 WAL, foreign key 활성화, normal synchronous 모드로 엽니다. monitor 설정의
CAS/quota 쓰기는 상태를 읽기 전에 write transaction을 확보합니다. PostgreSQL은 대응
경로에서 SQL transaction과 row lock을 사용합니다.

마이그레이션은 각 구현에 들어 있는 startup DDL입니다. 관계형 repository 공통
`meta` schema-version 테이블은 없습니다. SQLite는 cursor table rebuild와 additive
`ALTER TABLE`을 수행하고 PostgreSQL은 구현된 위치에서 `CREATE TABLE IF NOT EXISTS`와
`ADD COLUMN IF NOT EXISTS`를 사용합니다.

현재 테스트는 여러 HTTP/monitor 경로를 실제 임시 SQLite DB로 실행합니다.
PostgreSQL에는 연결하지 않으므로 PostgreSQL parity와 migration은 자동 통합 테스트로
검증되지 않았습니다.

## 이벤트 스토리지

### Fjall

Fjall은 event row와 message/user/session index를 저장합니다. 전체 event key는 big-endian
`ts_ms`로 시작하므로 all-user scan은 요청 시작점으로 seek합니다. single-user scan은
user-prefix index를 쓰지만 현재 해당 사용자의 history 처음부터 시작해 `since_ms` 이전
행을 건너뜁니다. 최근 쿼리도 그 사용자의 전체 history에 O(n)입니다. team scan은
member stream을 전역 시간순으로 merge하지 않고 member 목록 순으로 이어 붙입니다.

Fjall은 event schema version을 저장합니다. 호환되지 않는 persisted version은 event
keyspace를 비우고 client 재동기화에 의존합니다. wire schema version 및 관계형 DDL과
별개입니다.

### ClickHouse

ClickHouse는 시작 시 `toki_events`와 `toki_windows`를 만듭니다. event query는
`FINAL`, time/user predicate, `ORDER BY ts_ms`, caller가 준 `LIMIT`을 사용합니다.
동기 HTTP adapter는 blocking task에서 실행합니다.

두 업그레이드 경로를 구분해야 합니다.

- `toki_windows`는 `ReplacingMergeTree(observed_ts_ms)`에서 `updated_at` version
  column으로 자동 rebuild하며 중단된 rename 복구도 합니다.
- `toki_events`는 `session` column을 additive migration하지만 이전
  `(device_id, msg_id)` sort key를 `(device_id, provider, msg_id)`로 바꾸는 migration이
  **없습니다**. provider 혼용 전에 운영자가 기존 table을 확인하고 rebuild해야 합니다.

이 저장소 테스트는 ClickHouse 생성/마이그레이션을 실제 서버에서 실행하지 않습니다.

## Rate-limit windows

window row는 device가 아닌 account level입니다. 키는
`(user, provider, limit_id, account, kind, window_end_ms)`입니다. 고정 window가 시간에
따라 바뀌므로 client가 최근 전체 set을 재전송합니다. 서버는 가능한 필드를 단조롭게
merge하고(peak=max, flag=OR, first-seen=min), live field는 최신 observation을 씁니다.

`active_ms`, sample count, sampled-active fraction은 cross-device event union에서 다시
계산하지 않고 `max`로 merge합니다. 현재 의도적인 근사이며 multi-device 활동을
과소계산할 수 있습니다. window query는 `scope=self`만 지원하고 730일보다 오래된
row는 주기적으로 정리합니다.

Fjall은 window mutation을 프로세스 안에서 직렬화합니다. ClickHouse는 read/merge/write
주위에 user별 프로세스 로컬 lock을 둡니다. 여러 server replica는 race할 수 있고
client가 나중에 재전송할 때까지 contribution 하나를 잃을 수 있습니다.

## HTTP 쿼리 실행

`GET /api/v1/toki/query`는 [API.ko.md](API.ko.md)의 제한된 부분집합을 받습니다.
EventStore 범위는 `[start_ms, end_ms)`입니다. 날짜 전용 end는 다음 현지 자정으로
변환하며 exact numeric/datetime end는 배타입니다. RFC 3339와 dashed date는 원격에서
받지 않습니다.

리소스 가드:

| 가드 | 현재 값 | 중요 제약 |
|---|---:|---|
| 시간 버킷 | 2,000 | 너무 작은 `step` 거부 |
| 입력 이벤트 | 200,000 | 하드 상한, cursor 없음, aggregate truncation 전파 안 됨 |
| 집계 그룹 | 50,000 | 새 group을 버리면 최상위 `truncated` 추가 |
| ClickHouse 버퍼 body | 10 MiB | event 상한 전에 `502` 가능 |

bare raw `events`는 event-cap truncation을 응답 최상위에 표시합니다. 현재 toki CLI
변환은 그 flag를 버립니다. aggregate는 event-cap flag를 전달받지 않아 합계가 조용히
부분값일 수 있습니다. 이 cap은 일부 memory 사용을 제한하지만 pagination이나 response
streaming은 아닙니다.

## 인증과 보안

- access/refresh token은 서명 JWT입니다.
- refresh token은 일회 회전하며 ID/revocation 상태를 저장합니다.
- 비밀번호 변경은 해당 사용자의 모든 refresh token을 폐기합니다.
- 구현된 password, registration, device-code, refresh 경로에 brute-force/rate guard가
  있습니다.
- 비활성 사용자는 공유 short-TTL cache를 통해 HTTP와 TCP에서 거부됩니다.
- query scope는 인증 subject, team membership, role, 동적 maximum scope로 결정합니다.
- monitor 설정은 version CAS, 항목/총량 quota, 프로세스 로컬 write budget을 가진
  user별 opaque string입니다.
- prompt와 model response는 `ServerEvent` 또는 sync wire item에 포함되지 않습니다.

서비스 자체는 TLS를 종단하지 않습니다. `trust_proxy=true`는 직접 peer가 통제된
proxy일 때만 사용하세요.

## 장애 및 복구 성질

- client history가 원격 event 손실의 복구 원본입니다.
- cursor보다 event를 먼저 쓰므로 crash는 승인된 데이터 손실보다 replay를 유발합니다.
- device/account 삭제는 in-flight write를 잡기 위해 관계형 소유 row 삭제 전후로
  event/window purge를 시도합니다.
- Fjall과 ClickHouse 전환은 데이터를 이전하지 않습니다.
- 관계형 user/device/cursor 손실은 재인증 및 명시적 cursor reset/전체 sync가 필요할 수
  있습니다.
- ClickHouse mutation은 DB 수준에서 비동기이며 운영 backup/recovery 계획에 포함해야
  합니다.

## 검증 및 릴리즈 상태

커밋 `5804fc2`에서 Rust/Cargo 1.92.0은 116개 테스트를 나열합니다. protocol
frame/handler, parsing/aggregation, pricing, Fjall, 임시 SQLite를 사용한 HTTP/monitor
경로를 포함합니다. 실제 PostgreSQL/ClickHouse 통합 테스트, Compose 통합 테스트,
실제 ClickHouse migration 테스트는 0개입니다.

2.2.0 Cargo 패키지는 공개된 `toki-sync-protocol` v1.1.0 태그를 고정하며 sibling
patch 없이 빌드됩니다. Docker 소스 빌드는 릴리즈 검증에 포함됩니다.
