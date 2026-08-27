# toki-sync 설정 레퍼런스

바이너리 기본 설정 경로는 `./config.toml`입니다. `--config <path>`가
`TOKI_SYNC_CONFIG`보다 우선합니다. 번들 Compose 서비스는
`TOKI_SYNC_CONFIG=/etc/toki-sync/config.toml`을 지정하고
`config/toki-sync.toml`을 그 위치에 마운트합니다. `${VAR_NAME}`으로 참조한 환경
변수가 설정되지 않으면 빈 문자열로 확장됩니다.

## 예시 설정

```toml
[server]
# bind = "0.0.0.0"
tcp_port = 9090
http_port = 9091
# external_url = "${TOKI_EXTERNAL_URL}"
# trust_proxy = false
# max_concurrent_writes = 10

[auth]
jwt_secret = "${JWT_SECRET}"
# access_token_ttl_secs = 3600
# refresh_token_ttl_secs = 7776000
# brute_force_max_attempts = 5
# brute_force_window_secs = 300
# brute_force_lockout_secs = 900
# registration_mode = "closed"

[storage]
backend = "sqlite"
db_path = "/data/toki_sync.db"

[events]
backend = "fjall"
fjall_path = "/data/events.fjall"
# dedup_retention_secs = 2592000
# backend = "clickhouse"
# clickhouse_url = "http://clickhouse:8123"

[features]
# max_query_scope = "self"   # "self" | "team" | "all"

[log]
level = "info"
json = true
```

---

## Server 섹션

`[server]` 섹션.

| 키 | 타입 | 기본값 | 설명 |
|----|------|--------|------|
| `bind` | string | `0.0.0.0` | 바인드할 네트워크 인터페이스 |
| `http_port` | integer | `9091` | HTTP API 포트 (REST, 관리 콘솔, 쿼리 엔드포인트) |
| `tcp_port` | integer | `9090` | TCP 동기화 프로토콜 포트 (toki 데몬 연결) |
| `external_url` | string | *(빈값)* | JWT `iss`와 OIDC 리다이렉트 URI에 사용되는 공개 URL. 아래 참고 |
| `max_concurrent_writes` | integer | `10` | 이벤트 스토어 동시 배치 쓰기 최대 수 |
| `trust_proxy` | boolean | `false` | `X-Forwarded-For` / `X-Real-IP` 헤더 신뢰 여부. 아래 참고 |

#### `external_url`

JWT `iss` 클레임과 OIDC 리다이렉트 URI 도출에 사용됩니다. 예: `https://sync.example.com`.

#### `max_concurrent_writes`

여러 디바이스가 동시에 동기화할 때 thundering-herd 압력을 제한합니다. 한도를 넘는 배치 쓰기는 큐잉됩니다.

#### `trust_proxy`

`true`일 때 서버가 프록시 헤더에서 클라이언트 IP를 읽어 무차별 대입 추적에 사용합니다. 신뢰할 수 있는 리버스 프록시 뒤에 있을 때만 활성화하세요. 그렇지 않으면 클라이언트가 IP를 위조할 수 있습니다.

---

## Auth 섹션

`[auth]` 섹션.

| 키 | 타입 | 기본값 | 설명 |
|----|------|--------|------|
| `jwt_secret` | string | — | **필수.** HS256 서명 키. 아래 참고 |
| `access_token_ttl_secs` | integer | `3600` | 액세스 토큰 수명 (초, 기본: 1시간) |
| `refresh_token_ttl_secs` | integer | `7776000` | 리프레시 토큰 수명 (초, 기본: 90일) |
| `brute_force_max_attempts` | integer | `5` | 잠금 전 최대 로그인 실패 횟수 |
| `brute_force_window_secs` | integer | `300` | 추적 윈도우 (기본: 5분) |
| `brute_force_lockout_secs` | integer | `900` | 잠금 기간 (기본: 15분) |
| `registration_mode` | string | `"closed"` | 셀프 가입 정책. 아래 참고 |
| `oidc_issuer` | string | *(빈값)* | OIDC 프로바이더 URL (예: `https://accounts.google.com`) |
| `oidc_client_id` | string | *(빈값)* | ID 프로바이더에서 발급한 OIDC 클라이언트 ID |
| `oidc_client_secret` | string | *(빈값)* | OIDC 클라이언트 시크릿 |
| `oidc_redirect_uri` | string | *(빈값)* | OIDC 콜백 URL (예: `https://sync.example.com/auth/callback`) |

#### `jwt_secret`

`${JWT_SECRET}`으로 환경변수에서 읽을 수 있습니다. `openssl rand -base64 32`로 강한 값을 생성하세요.

#### `registration_mode`

`POST /register`를 통한 셀프 가입의 세 가지 정책:

- `"open"` — 누구나 가입 가능.
- `"approval"` — 가입은 pending 상태로 생성되며, 관리자가 `/admin/pending/:id/approve`로 승인해야 합니다.
- `"closed"` — 관리자만 `/admin/users`로 사용자 생성 가능.

### 무차별 대입 방지

로그인 실패는 IP + 사용자명 쌍 단위로 추적됩니다. `brute_force_window_secs` 내에 `brute_force_max_attempts`를 초과하면 해당 쌍이 `brute_force_lockout_secs` 동안 잠깁니다. `/login`, `/register`, `/token/refresh`에 적용됩니다.

### OIDC 설정

OIDC(Google, GitHub 등)를 활성화하려면 4개의 OIDC 필드를 모두 설정합니다. 서버는 시작 시 표준 OIDC discovery를 수행하고 결과를 1시간 TTL로 캐시합니다.

```toml
[auth]
jwt_secret = "${JWT_SECRET}"
oidc_issuer = "https://accounts.google.com"
oidc_client_id = "${OIDC_CLIENT_ID}"
oidc_client_secret = "${OIDC_CLIENT_SECRET}"
oidc_redirect_uri = "https://sync.example.com/auth/callback"
```

---

## Storage 섹션

`[storage]` 섹션.

| 키 | 타입 | 기본값 | 설명 |
|----|------|--------|------|
| `backend` | string | `sqlite` | 데이터베이스 백엔드: `sqlite` 또는 `postgres` |
| `sqlite_path` | string | `./data/toki_sync.db` | SQLite 파일 경로. `backend = "sqlite"`일 때 사용 |
| `db_path` | string | *(빈값)* | `sqlite_path`의 레거시 별칭. 아래 참고 |
| `postgres_url` | string | *(빈값)* | PostgreSQL 연결 문자열. 아래 참고 |

#### `db_path`

과거 설정은 `sqlite_path` 대신 `db_path`를 사용했습니다. 하위 호환을 위해 두 키가 공존합니다 — `db_path`가 설정되어 있고 `sqlite_path`가 기본값이면 `db_path`가 사용됩니다. 새 설정은 `sqlite_path`만 쓰세요.

#### `postgres_url`

`backend = "postgres"`일 때 사용. 예: `postgres://user:pass@host/dbname`.

### SQLite vs PostgreSQL

- **SQLite** (기본): 설정 불필요, 단일 파일 데이터베이스. 개인 사용 및 소규모 팀에 권장합니다.
- **PostgreSQL**: 대규모 팀에 더 나은 동시성. 별도 PostgreSQL 서버가 필요합니다.

```toml
# SQLite (기본)
[storage]
backend = "sqlite"
sqlite_path = "/data/toki_sync.db"

# PostgreSQL
[storage]
backend = "postgres"
postgres_url = "postgres://toki:password@db:5432/toki_sync"
```

---

## Events 섹션

`[events]` 섹션.

| 키 | 타입 | 기본값 | 설명 |
|----|------|--------|------|
| `backend` | string | `fjall` | 이벤트 스토어 백엔드: `fjall` (내장, 외부 의존성 없음) 또는 `clickhouse` (외부 ClickHouse 서버) |
| `fjall_path` | string | `/data/events.fjall` | Fjall 데이터베이스 디렉토리 경로. `backend = "fjall"`일 때 사용 |
| `clickhouse_url` | string | *(빈값)* | ClickHouse HTTP 엔드포인트. `backend = "clickhouse"`이면 필수 |
| `dedup_retention_secs` | integer | `2592000` | Fjall 중복 제거 인덱스 보존 기간(초, 기본 30일). 이벤트 행 자체는 삭제하지 않음 |

### Fjall vs ClickHouse

- **Fjall** (기본): 내장 LSM-tree 저장소. 현재 중복 제거 키는 `(device_id, provider, msg_id)`입니다.
- **ClickHouse**: `ReplacingMergeTree(ts_ms)`를 사용하는 외부 컬럼형 데이터베이스입니다. 구현되어 있지만 이 저장소에는 실제 ClickHouse 통합 테스트가 없습니다.

```toml
# Fjall (기본 — 외부 의존성 없음)
[events]
backend = "fjall"
fjall_path = "/data/events.fjall"

# ClickHouse (외부 ClickHouse 서버 필요)
[events]
backend = "clickhouse"
clickhouse_url = "http://clickhouse:8123"
```

위 두 값을 모두 설정한 뒤 `docker compose --profile clickhouse up -d`를 실행하세요.
프로필만 켜면 ClickHouse만 시작되고 toki-sync는 계속 Fjall을 사용합니다. 백엔드를
바꿔도 기존 이벤트는 복사되지 않습니다.

### ClickHouse 업그레이드 주의사항

새 `toki_events` 테이블은 `ORDER BY (device_id, provider, msg_id)`를 사용합니다.
기존 배포는 여전히 `ORDER BY (device_id, msg_id)`일 수 있습니다. 시작 시
`CREATE TABLE IF NOT EXISTS`를 사용하므로 기존 정렬 키를 다시 만들지 않으며, 같은
device/message ID를 가진 Claude Code와 Codex 행이 합쳐질 수 있습니다. 업그레이드
전에 확인하세요.

```sql
SELECT sorting_key
FROM system.tables
WHERE database = currentDatabase() AND name = 'toki_events';
```

이번 릴리즈에는 자동 `toki_events` 정렬 키 마이그레이션이 없습니다. 기존 테이블에서
provider를 혼용하기 전에 백업하고 테이블 재구축/전체 재동기화를 계획하세요. 별도의
`toki_windows.updated_at` 마이그레이션은 자동이지만, 이 저장소 테스트는 실제
ClickHouse 인스턴스에서 그 경로를 실행하지 않았습니다.

---

## Log 섹션

`[log]` 섹션.

| 키 | 타입 | 기본값 | 설명 |
|----|------|--------|------|
| `level` | string | `info` | 로그 레벨: `trace`, `debug`, `info`, `warn`, `error` |
| `json` | boolean | `false` | JSON 형식으로 로그 출력. 프로덕션 환경에서 권장 (구조화된 로깅) |

---

## Features 섹션

`[features]` 섹션.

| 키 | 타입 | 기본값 | 설명 |
|----|------|--------|------|
| `max_query_scope` | string | `"self"` | 비관리자가 `/api/v1/toki/query`에서 요청할 수 있는 최대 scope. `self`, `team`, `all` 중 하나. 관리자는 항상 `all` |

---

## 환경 변수

환경변수는 두 가지 방식으로 사용됩니다:
1. **TOML 내부**: `toki-sync.toml`에서 `${VAR_NAME}` 문법으로 값 확장
2. **`.env` 파일**: Docker Compose가 `.env`를 읽어 컨테이너에 변수를 주입

| 변수 | 필수 | 설명 |
|------|------|------|
| `TOKI_ADMIN_PASSWORD` | 초기 설정 | 내장 `admin`이 없을 때만 생성. 이후 환경 변수 변경은 비밀번호를 바꾸지 않음 |
| `JWT_SECRET` | 운영 필수 | TOML이 `${JWT_SECRET}`을 참조할 때의 JWT 서명 키. 생성: `openssl rand -base64 32` |
| `TOKI_EXTERNAL_URL` | 배포에 따라 | TOML이 참조할 때만 확장. Compose는 Caddy의 `TOKI_DOMAIN`으로도 전달 |
| `DUCKDNS_TOKEN` | 현재 효과 없음 | 예시/Compose에는 있지만 번들 Caddy 빌드와 Caddyfile은 DuckDNS DNS 모듈을 사용하지 않음 |
| `TOKI_VERSION` | - | Docker 이미지 태그 (기본: `latest`) |

### `.env` 예시

```bash
# 필수
TOKI_ADMIN_PASSWORD=your-strong-password
JWT_SECRET=base64-encoded-secret-here
TOKI_EXTERNAL_URL=https://yourserver.duckdns.org

# Caddy TLS (선택)
DUCKDNS_TOKEN=your-duckdns-token

# 이미지 버전 (선택)
TOKI_VERSION=2.2.0
```

> **보안**: `.env`를 커밋하지 마세요. 번들 Caddyfile은 현재 내부 CA를 강제하며
> 공개 Let's Encrypt/DuckDNS 인증서를 발급하지 않습니다.

---

## 설정 로딩

서버는 다음 순서로 설정을 로드합니다.

1. `--config`, `TOKI_SYNC_CONFIG`, 기본 `./config.toml` 순으로 경로 선택.
2. `${VAR_NAME}` 플레이스홀더를 환경 변수 값으로 확장.
3. TOML을 설정 구조체로 파싱.
4. 누락된 필드에 기본값 적용.

선택한 파일이 없으면 서버는 내장 기본값을 사용하고 환경 변수의 `JWT_SECRET`을
읽습니다. 미설정 시 경고와 함께 `change-me-in-production`을 사용합니다. 파일이
존재하면 환경 변수 확장 후 `[auth]`와 `jwt_secret`이 있어야 합니다.

## 검증 상태

현재 116개 테스트는 설정 파싱, SQLite 기반 HTTP 경로, Fjall을 검사합니다.
PostgreSQL과 ClickHouse는 컴파일되지만 이 저장소에는 실제 인스턴스 통합 테스트가
없습니다. 저장소 Docker 소스 빌드도 protocol v1.1.0 태그와 임시 형제 patch 제거
전까지 막혀 있습니다. 선택 백엔드와 실제 마이그레이션은 운영과 유사한 폐기 가능한
인스턴스에서 실행하기 전까지 미검증으로 보세요.
