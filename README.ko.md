<p align="center">
  <img src="assets/logo.png" alt="toki-sync 로고" width="160" />
</p>

<h1 align="center">toki-sync</h1>

<p align="center">
  <b>멀티 디바이스 토큰 사용량 동기화 서버</b><br>
  모든 기기의 AI 도구 사용량을 수집하고, VictoriaMetrics에 시계열 저장, 통합 대시보드를 제공합니다.
</p>

<p align="center">
  <a href="https://github.com/korjwl1/toki">toki</a> 생태계의 일부입니다.
</p>

<p align="center">
  <a href="README.md">🇺🇸 English</a>
</p>

---

## 목차

- [Quick Start](#quick-start)
- [아키텍처](#아키텍처)
- [기능](#기능)
- [설정](#설정)
- [배포](#배포)
- [클라이언트 설정](#클라이언트-설정)
- [API 레퍼런스](#api-레퍼런스)
- [기술 스택](#기술-스택)
- [라이선스](#라이선스)

---

## Quick Start

### 사전 요구사항

- Docker 및 Docker Compose v2
- 서버를 가리키는 도메인 이름 (예: `yourserver.duckdns.org`)

### 1. 클론 및 설정

```bash
git clone https://github.com/korjwl1/toki-sync.git
cd toki-sync

cp .env.example .env
cp config/toki-sync.toml.example config/toki-sync.toml
```

`.env`를 열고 필수 값을 입력합니다:

```bash
TOKI_ADMIN_PASSWORD=your-strong-password
TOKI_JWT_SECRET=$(openssl rand -base64 32)
TOKI_EXTERNAL_URL=https://yourserver.duckdns.org
```

### 2. 배포

```bash
# Caddy 사용 (Let's Encrypt 자동 TLS)
echo "DUCKDNS_TOKEN=your-duckdns-token" >> .env
docker compose --profile caddy up -d

# Caddy 미사용 (기존 리버스 프록시 사용)
docker compose up -d
```

### 3. 디바이스 연결

```bash
# toki가 설치된 아무 기기에서
toki sync enable --server yourserver.duckdns.org:9090 --username admin
toki sync status
```

완료. 토큰 사용량이 자동으로 동기화됩니다.

---

## 아키텍처

```
[디바이스 A]  [디바이스 B]  [디바이스 C]
toki daemon   toki daemon   toki daemon
     └── TCP+TLS (bincode) ──┐
                              v
                      toki-sync 서버
                      ├── TCP :9090 (동기화 프로토콜)
                      ├── HTTP :9091 (인증 + PromQL 프록시 + 대시보드)
                      └── SQLite / PostgreSQL
                              │
                      VictoriaMetrics
                      (시계열 저장소)
```

- **TCP :9090** — 바이너리 동기화 프로토콜. toki 데몬이 persistent TLS 연결을 유지하고, 이벤트를 배치로 모아 (1,000/배치), zstd 압축 (100개 이상 시), ACK 기반 흐름 제어로 전송합니다.
- **HTTP :9091** — 인증, PromQL 프록시, 디바이스 관리, 관리자, 팀, 웹 대시보드를 위한 REST API. JWT 인증.
- **VictoriaMetrics** — 모든 시계열 데이터를 저장합니다. 사용자별 label injection으로 데이터 격리를 보장하는 PromQL 프록시를 통해 조회합니다.

---

## 기능

- **멀티 디바이스 동기화** — TCP 바이너리 프로토콜, zstd 압축, ACK 흐름 제어, 재연결 시 delta-sync
- **JWT 인증** — 비밀번호 기반 및 OIDC (Google, GitHub 등) 로그인 플로우
- **PromQL 프록시** — 사용자별 label injection으로 데이터 격리. toki CLI `--remote` 쿼리 및 Toki Monitor 서버 모드와 호환
- **웹 대시보드** — 4개 차트 패널, 시간 범위 선택, 디바이스 목록, 팀 뷰
- **팀 / 조직** — 팀 멤버 간 집계 쿼리
- **듀얼 데이터베이스 백엔드** — SQLite (기본, 설정 불필요) 또는 PostgreSQL (대규모 운영용)
- **Docker 배포** — Caddy 프로필로 자동 TLS, 또는 기존 리버스 프록시 사용
- **무차별 대입 방지** — 시도 횟수 제한, 잠금 윈도우, IP 기반 추적
- **리프레시 토큰 로테이션** — 일회용 로테이션으로 안전한 토큰 갱신
- **글로벌 배치 스로틀링** — VictoriaMetrics 동시 쓰기 제한으로 thundering herd 방지

---

## 설정

서버 설정은 `config/toki-sync.toml`에 있습니다. `${VAR_NAME}` 문법으로 환경변수를 확장할 수 있습니다.

### `[server]`

| 키 | 기본값 | 설명 |
|----|--------|------|
| `bind` | `0.0.0.0` | 바인드 주소 |
| `http_port` | `9091` | HTTP API 포트 |
| `tcp_port` | `9090` | TCP 동기화 프로토콜 포트 |
| `external_url` | — | JWT `iss` 및 OIDC 리다이렉트용 공개 URL |
| `max_concurrent_writes` | `10` | VictoriaMetrics 동시 배치 쓰기 최대 수 |

### `[auth]`

| 키 | 기본값 | 설명 |
|----|--------|------|
| `jwt_secret` | — | **필수.** HS256 서명 키 |
| `access_token_ttl_secs` | `3600` | 액세스 토큰 수명 (1시간) |
| `refresh_token_ttl_secs` | `2592000` | 리프레시 토큰 수명 (30일) |
| `brute_force_max_attempts` | `5` | 잠금 전 실패 허용 횟수 |
| `brute_force_window_secs` | `300` | 추적 윈도우 (5분) |
| `brute_force_lockout_secs` | `900` | 잠금 기간 (15분) |
| `allow_registration` | `false` | 셀프 회원가입 허용 여부 |
| `oidc_issuer` | — | OIDC 프로바이더 URL (빈값 = 비활성화) |
| `oidc_client_id` | — | OIDC 클라이언트 ID |
| `oidc_client_secret` | — | OIDC 클라이언트 시크릿 |
| `oidc_redirect_uri` | — | OIDC 콜백 URL |

### `[storage]`

| 키 | 기본값 | 설명 |
|----|--------|------|
| `backend` | `sqlite` | `sqlite` 또는 `postgres` |
| `sqlite_path` | `./data/toki_sync.db` | SQLite 데이터베이스 파일 경로 |
| `postgres_url` | — | PostgreSQL 연결 문자열 |

### `[backend]`

| 키 | 기본값 | 설명 |
|----|--------|------|
| `vm_url` | `http://victoriametrics:8428` | VictoriaMetrics 엔드포인트 |

### `[log]`

| 키 | 기본값 | 설명 |
|----|--------|------|
| `level` | `info` | 로그 레벨 (trace, debug, info, warn, error) |
| `json` | `false` | JSON 로그 형식 |

### 환경변수

| 변수 | 필수 | 설명 |
|------|------|------|
| `TOKI_ADMIN_PASSWORD` | O | 관리자 계정 비밀번호 (첫 시작 시 생성) |
| `TOKI_JWT_SECRET` | O | JWT 서명 키. 생성: `openssl rand -base64 32` |
| `TOKI_EXTERNAL_URL` | O | 공개 URL (예: `https://yourserver.duckdns.org`) |
| `DUCKDNS_TOKEN` | Caddy만 | Let's Encrypt DNS 챌린지용 DuckDNS 토큰 |
| `TOKI_VERSION` | X | Docker 이미지 태그 (기본: `latest`) |

---

## 배포

### 시나리오 A: Caddy 사용 (원클릭 TLS)

기존 리버스 프록시가 없는 서버에 적합합니다. Caddy가 Let's Encrypt를 통해 TLS 인증서를 자동으로 관리합니다.

```bash
echo "DUCKDNS_TOKEN=your-duckdns-token" >> .env
docker compose --profile caddy up -d
```

3개의 컨테이너가 시작됩니다:
- **toki-sync-server** — 동기화 프로토콜 (TCP :9090) + 인증 API (HTTP :9091)
- **VictoriaMetrics** — 시계열 저장소 (내부 전용, 외부 노출 안 함)
- **Caddy** — TLS 종단, :443 (HTTPS)과 :9090 (TLS-wrapped TCP) 노출

### 시나리오 B: Caddy 미사용 (기존 리버스 프록시)

이미 nginx, Traefik 등으로 TLS를 처리하고 있다면:

```bash
docker compose up -d
```

toki-sync-server와 VictoriaMetrics만 시작됩니다. 기존 프록시에서 다음을 포워딩하세요:

| 트래픽 | 업스트림 |
|--------|----------|
| HTTPS :443 | `http://127.0.0.1:9091` |
| TLS :9090 (TCP 스트림) | `127.0.0.1:9090` |

nginx 설정 예시:

```nginx
# HTTP API
server {
    listen 443 ssl;
    server_name yourserver.example.com;
    location / {
        proxy_pass http://127.0.0.1:9091;
    }
}

# TCP 동기화 (stream 모듈)
stream {
    server {
        listen 9090 ssl;
        proxy_pass 127.0.0.1:9090;
    }
}
```

참고: 시나리오 B에서는 `docker-compose.yml`의 `toki-sync-server`에 포트 매핑을 추가하세요:

```yaml
ports:
  - "9091:9091"
  - "9090:9090"
```

### 시나리오 C: 자체 서명 TLS (IP 전용 서버)

도메인 이름이 없는 서버(예: 로컬 IP의 홈 랩):

```bash
docker compose up -d
```

클라이언트는 `--insecure` 플래그로 자체 서명 인증서를 수락합니다:

```bash
toki sync enable --server 1.2.3.4:9090 --insecure --username admin
```

### 데이터 영속성

| 볼륨 | 경로 | 내용 | 손실 시 |
|------|------|------|---------|
| `toki-data` | `/data` | SQLite (사용자, 디바이스, 커서) | 재로그인 + 전체 재동기화 필요 |
| `vm-data` | `/vm-data` | VictoriaMetrics 시계열 데이터 | **복구 불가** |
| `caddy-data` | `/data` | Let's Encrypt 인증서 | 자동 재발급 (주당 5회 제한) |

### 백업

`vm-data`가 핵심 볼륨입니다. VictoriaMetrics는 핫 스냅샷을 지원합니다:

```bash
# 스냅샷 생성 (다운타임 없음)
docker exec victoriametrics wget -qO- http://localhost:8428/snapshot/create

# 스냅샷 목록
docker exec victoriametrics wget -qO- http://localhost:8428/snapshot/list
```

백업 접근을 쉽게 하려면 named volume 대신 bind mount를 사용하세요:

```yaml
volumes:
  - ./data/vm:/vm-data
  - ./data/toki:/data
```

자세한 내용은 [VictoriaMetrics 백업 문서](https://docs.victoriametrics.com/single-server-victoriametrics/#backups)를 참고하세요.

---

## 클라이언트 설정

[toki](https://github.com/korjwl1/toki)가 설치된 각 기기에서:

```bash
# 시나리오 A/B: 유효한 TLS 인증서가 있는 도메인
toki sync enable --server yourserver.duckdns.org:9090 --username admin

# 시나리오 C: 자체 서명 TLS (IP 전용)
toki sync enable --server 1.2.3.4:9090 --insecure --username admin

# 연결 확인
toki sync status

# 등록된 모든 디바이스 목록
toki sync devices

# CLI에서 서버 데이터 쿼리
toki report query --remote 'sum by (model)(toki_tokens_total)'

# 동기화 비활성화
toki sync disable
```

toki 데몬이 자동으로 토큰 사용량 데이터를 배치로 모아 동기화합니다. 연결이 끊기면 이벤트가 로컬에 누적되고, 재연결 시 delta-sync됩니다.

GUI로 보려면 [Toki Monitor](https://github.com/korjwl1/toki-monitor)를 사용하세요 — 대시보드 툴바에서 로컬/서버 모드를 전환할 수 있습니다.

---

## API 레퍼런스

모든 HTTP 엔드포인트는 포트 9091에서 제공됩니다. JWT 인증이 필요한 엔드포인트는 `Authorization: Bearer <token>` 헤더가 필요합니다.

### 공개

| 메서드 | 경로 | 설명 |
|--------|------|------|
| `GET` | `/health` | 헬스 체크 |
| `POST` | `/login` | 인증 (사용자명 + 비밀번호), JWT 반환 |
| `POST` | `/register` | 셀프 회원가입 (`allow_registration` 활성화 시) |
| `POST` | `/token/refresh` | 액세스 토큰 갱신 |
| `POST` | `/auth-method` | 사용자명에 사용 가능한 인증 방식 확인 |

### OIDC

| 메서드 | 경로 | 설명 |
|--------|------|------|
| `GET` | `/auth/oidc/authorize` | OIDC 로그인 플로우 시작 |
| `GET` | `/auth/callback` | OIDC 콜백 핸들러 |

### PromQL 프록시 (JWT 필수)

| 메서드 | 경로 | 설명 |
|--------|------|------|
| `GET` | `/api/v1/query` | 즉시 PromQL 쿼리 (사용자별 label injection) |
| `GET` | `/api/v1/query_range` | 범위 PromQL 쿼리 (사용자별 label injection) |

### 사용자 셀프서비스 (JWT 필수)

| 메서드 | 경로 | 설명 |
|--------|------|------|
| `GET` | `/me/devices` | 자신의 디바이스 목록 |
| `DELETE` | `/me/devices/:device_id` | 디바이스 제거 |
| `PATCH` | `/me/devices/:device_id/name` | 디바이스 이름 변경 |
| `PATCH` | `/me/password` | 비밀번호 변경 |
| `GET` | `/me/teams` | 자신의 팀 멤버십 목록 |

### 팀 (JWT 필수)

| 메서드 | 경로 | 설명 |
|--------|------|------|
| `GET` | `/api/v1/teams/:team_id/query_range` | 팀 집계 PromQL 쿼리 |

### 관리자 (JWT 필수, admin 역할)

| 메서드 | 경로 | 설명 |
|--------|------|------|
| `GET` | `/admin/users` | 전체 사용자 목록 |
| `POST` | `/admin/users` | 사용자 생성 |
| `DELETE` | `/admin/users/:user_id` | 사용자 삭제 |
| `PATCH` | `/admin/users/:user_id/password` | 사용자 비밀번호 변경 |
| `GET` | `/admin/devices` | 전체 디바이스 목록 |
| `DELETE` | `/admin/devices/:device_id` | 디바이스 삭제 |
| `GET` | `/admin/teams` | 전체 팀 목록 |
| `POST` | `/admin/teams` | 팀 생성 |
| `DELETE` | `/admin/teams/:team_id` | 팀 삭제 |
| `GET` | `/admin/teams/:team_id/members` | 팀 멤버 목록 |
| `POST` | `/admin/teams/:team_id/members` | 팀 멤버 추가 |
| `DELETE` | `/admin/teams/:team_id/members/:user_id` | 팀 멤버 제거 |

### 대시보드

| 메서드 | 경로 | 설명 |
|--------|------|------|
| `GET` | `/` | `/dashboard`로 리다이렉트 |
| `GET` | `/dashboard` | 웹 대시보드 (HTML) |
| `GET` | `/login` | 로그인 페이지 (HTML) |

---

## 문서

| 문서 | 설명 |
|------|------|
| **[설정 레퍼런스](docs/CONFIGURATION.ko.md)** | 전체 TOML 섹션, 필드, 기본값, 환경변수 |
| **[HTTP API 레퍼런스](docs/API.ko.md)** | 전체 엔드포인트, 요청/응답 예시, 인증 |

---

## 문서

| 문서 | 설명 |
|------|------|
| **[설정 레퍼런스](docs/CONFIGURATION.ko.md)** | 전체 TOML 섹션, 필드, 기본값, 환경변수 |
| **[HTTP API 레퍼런스](docs/API.ko.md)** | 전체 엔드포인트, 요청/응답 예시, 인증 |

---

## 기술 스택

| 용도 | 선택 | 근거 |
|------|------|------|
| HTTP 프레임워크 | axum 0.7 | 비동기, tower 미들웨어 생태계 |
| 비동기 런타임 | tokio | 전기능 비동기 I/O |
| 데이터베이스 | sqlx 0.8 (SQLite + PostgreSQL) | 컴파일 타임 쿼리 체크, 듀얼 백엔드 |
| 시계열 | VictoriaMetrics | PromQL 호환, 낮은 리소스 사용량 |
| 인증 | jsonwebtoken 9 + bcrypt | JWT 액세스/리프레시 토큰, 안전한 비밀번호 해싱 |
| OIDC | reqwest + 수동 discovery | 무거운 프레임워크 없는 표준 OIDC 플로우 |
| Sync 프로토콜 | toki-sync-protocol (공유 crate) | Wire-compatible 타입, bincode 직렬화 |
| 압축 | zstd 0.13 | 동기화 프로토콜용 고속 배치 압축 |
| 직렬화 | bincode (sync), serde_json (API), toml (config) | 성능을 위한 바이너리, 상호운용을 위한 JSON |
| 로깅 | tracing + tracing-subscriber | JSON 출력 옵션을 지원하는 구조화된 로깅 |
| 설정 | toml 0.8 + `${ENV}` 확장 | 간단하고 읽기 쉬운 서버 설정 |

---

## 라이선스

[FSL-1.1-Apache-2.0](LICENSE)
