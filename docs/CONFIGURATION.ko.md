# toki-sync 설정 레퍼런스

서버 설정은 `config/toki-sync.toml` 파일에 저장됩니다. `${VAR_NAME}` 문법으로 환경변수를 확장할 수 있습니다 — 변수가 설정되지 않으면 빈 문자열로 확장됩니다.

## 예시

```toml
[server]
# bind = "0.0.0.0"
tcp_port = 9090
http_port = 9091

[auth]
jwt_secret = "${JWT_SECRET}"
# access_token_ttl_secs = 3600
# refresh_token_ttl_secs = 2592000
# brute_force_max_attempts = 5
# brute_force_window_secs = 300
# brute_force_lockout_secs = 900
# allow_registration = false

[storage]
db_path = "/data/toki_sync.db"

[backend]
vm_url = "http://victoriametrics:8428"

[log]
level = "info"
json = true
```

---

## `[server]`

| 키 | 타입 | 기본값 | 설명 |
|----|------|--------|------|
| `bind` | string | `0.0.0.0` | 바인드할 네트워크 인터페이스 |
| `http_port` | integer | `9091` | HTTP API 포트 (REST, 대시보드, PromQL 프록시) |
| `tcp_port` | integer | `9090` | TCP 동기화 프로토콜 포트 (toki 데몬 연결) |
| `external_url` | string | *(빈값)* | JWT `iss` 클레임 및 OIDC 리다이렉트 URI 도출에 사용되는 공개 URL. 예: `https://sync.example.com` |
| `max_concurrent_writes` | integer | `10` | VictoriaMetrics 동시 배치 쓰기 최대 수. 여러 디바이스가 동시에 동기화할 때 thundering-herd 압력을 제한합니다 |

---

## `[auth]`

| 키 | 타입 | 기본값 | 설명 |
|----|------|--------|------|
| `jwt_secret` | string | — | **필수.** JWT 토큰 HS256 서명 키. `${JWT_SECRET}`으로 환경변수에서 읽을 수 있습니다. `openssl rand -base64 32`로 생성 |
| `access_token_ttl_secs` | integer | `3600` | 액세스 토큰 수명 (초 단위, 기본: 1시간) |
| `refresh_token_ttl_secs` | integer | `2592000` | 리프레시 토큰 수명 (초 단위, 기본: 30일) |
| `brute_force_max_attempts` | integer | `5` | 잠금 전 최대 로그인 실패 횟수 |
| `brute_force_window_secs` | integer | `300` | 실패 횟수 추적 윈도우 (기본: 5분) |
| `brute_force_lockout_secs` | integer | `900` | 최대 횟수 초과 후 잠금 기간 (기본: 15분) |
| `allow_registration` | boolean | `false` | `POST /register`를 통한 셀프 회원가입 허용. `false`이면 관리자만 사용자를 생성할 수 있습니다 |
| `oidc_issuer` | string | *(빈값)* | OIDC 프로바이더 URL (예: `https://accounts.google.com`). 빈값 = OIDC 비활성화 |
| `oidc_client_id` | string | *(빈값)* | ID 프로바이더에서 발급한 OIDC 클라이언트 ID |
| `oidc_client_secret` | string | *(빈값)* | OIDC 클라이언트 시크릿 |
| `oidc_redirect_uri` | string | *(빈값)* | OIDC 콜백 URL (예: `https://sync.example.com/auth/callback`) |

### 무차별 대입 방지

무차별 대입 방지는 IP + 사용자명 조합별로 로그인 실패 횟수를 추적합니다. `brute_force_window_secs` 내에 `brute_force_max_attempts`를 초과하면 해당 IP+사용자명 쌍이 `brute_force_lockout_secs` 동안 잠깁니다. `/login`, `/register`, `/token/refresh` 엔드포인트에 적용됩니다.

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

## `[storage]`

| 키 | 타입 | 기본값 | 설명 |
|----|------|--------|------|
| `backend` | string | `sqlite` | 데이터베이스 백엔드: `sqlite` 또는 `postgres` |
| `sqlite_path` | string | `./data/toki_sync.db` | SQLite 데이터베이스 파일 경로. `backend = "sqlite"`일 때 사용 |
| `db_path` | string | *(빈값)* | `sqlite_path`의 레거시 별칭 (하위 호환). 설정되어 있고 `sqlite_path`가 기본값이면 이 값을 사용 |
| `postgres_url` | string | *(빈값)* | PostgreSQL 연결 문자열. `backend = "postgres"`일 때 사용. 예: `postgres://user:pass@host/dbname` |

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

## `[backend]`

| 키 | 타입 | 기본값 | 설명 |
|----|------|--------|------|
| `vm_url` | string | `http://victoriametrics:8428` | VictoriaMetrics HTTP 엔드포인트. 서버가 시계열 데이터를 여기에 쓰고 PromQL 쿼리를 프록시합니다 |

Docker Compose에서는 VictoriaMetrics가 `victoriametrics`라는 서비스명으로 실행되므로 기본 URL이 그대로 동작합니다. Docker 외부에서 실행할 때는 실제 VictoriaMetrics 주소로 변경하세요.

---

## `[log]`

| 키 | 타입 | 기본값 | 설명 |
|----|------|--------|------|
| `level` | string | `info` | 로그 레벨: `trace`, `debug`, `info`, `warn`, `error` |
| `json` | boolean | `false` | JSON 형식으로 로그 출력. 프로덕션 환경에서 권장 (구조화된 로깅) |

---

## 환경변수

환경변수는 두 가지 방식으로 사용됩니다:
1. **TOML 내부**: `toki-sync.toml`에서 `${VAR_NAME}` 문법으로 값 확장
2. **`.env` 파일**: Docker Compose가 `.env`를 읽어 컨테이너에 변수를 주입

| 변수 | 필수 | 설명 |
|------|------|------|
| `TOKI_ADMIN_PASSWORD` | O | 관리자 계정 비밀번호. 첫 서버 시작 시 자동 생성 |
| `JWT_SECRET` | O | JWT 서명 키. 생성: `openssl rand -base64 32` |
| `TOKI_EXTERNAL_URL` | O | 공개 URL (예: `https://yourserver.duckdns.org`). JWT `iss` 및 OIDC 리다이렉트에 사용 |
| `DUCKDNS_TOKEN` | Caddy 프로필만 | Let's Encrypt DNS-01 챌린지용 DuckDNS API 토큰 |
| `TOKI_VERSION` | X | Docker 이미지 태그 (기본: `latest`) |

### `.env` 예시

```bash
# 필수
TOKI_ADMIN_PASSWORD=your-strong-password
JWT_SECRET=base64-encoded-secret-here
TOKI_EXTERNAL_URL=https://yourserver.duckdns.org

# Caddy TLS (선택)
DUCKDNS_TOKEN=your-duckdns-token

# 이미지 버전 (선택)
TOKI_VERSION=0.1.0
```

> **보안**: `.env` 파일을 버전 관리에 커밋하지 마세요. `.env.example` 파일이 템플릿으로 제공됩니다.

---

## 설정 로딩

서버는 다음 순서로 설정을 로드합니다:

1. `config/toki-sync.toml` 읽기 (`--config` 플래그로 경로 지정 가능)
2. `${VAR_NAME}` 플레이스홀더를 환경변수 값으로 확장
3. TOML을 설정 구조체로 파싱
4. 누락된 필드에 기본값 적용

설정 파일이 없으면 환경변수에서 `JWT_SECRET`을 읽고 나머지는 내장 기본값을 사용합니다 (JWT_SECRET이 미설정이면 `change-me-in-production`으로 대체).
