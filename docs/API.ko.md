# toki-sync HTTP API 레퍼런스

모든 HTTP 엔드포인트는 포트 9091에서 제공됩니다 (`[server].http_port`로 설정 가능).

## 인증

JWT 인증이 필요한 엔드포인트는 `Authorization` 헤더가 필요합니다:

```http
Authorization: Bearer <access_token>
```

액세스 토큰은 `access_token_ttl_secs` 후에 만료됩니다 (기본: 1시간). `/token/refresh` 엔드포인트로 새로운 토큰 쌍을 받을 수 있습니다.

모든 에러 응답은 다음 형식을 따릅니다:

```json
{ "error": "에러 메시지" }
```

---

## 공개 엔드포인트

### `GET /health`

헬스 체크.

**응답** `200 OK`

```json
{ "status": "ok" }
```

---

### `POST /login`

사용자명과 비밀번호로 인증합니다. JWT 액세스 토큰과 리프레시 토큰을 반환합니다.

**요청 본문**

```json
{
  "username": "admin",
  "password": "your-password",
  "device_id": "macbook-pro"
}
```

| 필드 | 타입 | 필수 | 설명 |
|------|------|------|------|
| `username` | string | 필수 | 계정 사용자명 |
| `password` | string | 필수 | 계정 비밀번호 |
| `device_id` | string | - | 디바이스 식별자 (디바이스별 관리를 위해 리프레시 토큰에 포함) |

**응답** `200 OK`

```json
{
  "access_token": "eyJhbGciOi...",
  "refresh_token": "eyJhbGciOi...",
  "token_type": "Bearer",
  "expires_in": 3600
}
```

**에러**

| 상태 | 메시지 | 설명 |
|------|--------|------|
| `401` | `invalid credentials` | 잘못된 사용자명 또는 비밀번호 |
| `401` | `this account uses OIDC login` | OIDC 계정은 비밀번호 로그인 불가 |
| `429` | `too many attempts, retry after Ns` | 무차별 대입 잠금 활성 상태 |

---

### `POST /register`

새 사용자 계정을 셀프 등록합니다. 설정에서 `registration_mode = "open"` 또는 `registration_mode = "approval"`일 때만 사용 가능합니다.

**요청 본문**

```json
{
  "username": "newuser",
  "password": "strong-password"
}
```

| 필드 | 타입 | 필수 | 제약 |
|------|------|------|------|
| `username` | string | 필수 | 3-32자, 영숫자 + `_`, `-`, `.` |
| `password` | string | 필수 | 8-128자 |

**응답** `201 Created`

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "username": "newuser"
}
```

**에러**

| 상태 | 메시지 | 설명 |
|------|--------|------|
| `403` | `registration is disabled` | `registration_mode`이 `"closed"` |
| `409` | `username already exists` | 중복된 사용자명 |
| `422` | `username must be 3-32 characters` | 잘못된 사용자명 길이 |
| `422` | `password must be 8-128 characters` | 잘못된 비밀번호 길이 |

---

### `POST /token/refresh`

리프레시 토큰을 사용하여 액세스 토큰을 갱신합니다. 일회용 로테이션을 구현합니다: 기존 리프레시 토큰은 무효화되고 새로운 토큰 쌍이 반환됩니다.

**요청 본문**

```json
{
  "refresh_token": "eyJhbGciOi...",
  "device_id": "macbook-pro"
}
```

| 필드 | 타입 | 필수 | 설명 |
|------|------|------|------|
| `refresh_token` | string | 필수 | 현재 리프레시 토큰 |
| `device_id` | string | - | 디바이스 식별자 |

**응답** `200 OK`

```json
{
  "access_token": "eyJhbGciOi...",
  "refresh_token": "eyJhbGciOi...",
  "token_type": "Bearer",
  "expires_in": 3600
}
```

**에러**

| 상태 | 메시지 | 설명 |
|------|--------|------|
| `401` | `invalid or expired refresh token` | 토큰이 만료되었거나, 이미 사용되었거나, 유효하지 않음 |

---

### `POST /auth-method`

사용자명에 대해 사용 가능한 인증 방식을 확인합니다. 서버 설정에 따라 `"password"` 또는 `"oidc"`를 반환합니다.

**요청 본문**

```json
{
  "username": "admin"
}
```

**응답** `200 OK` (비밀번호 인증)

```json
{ "method": "password" }
```

**응답** `200 OK` (OIDC 설정됨)

```json
{
  "method": "oidc",
  "auth_url": "/auth/oidc/authorize?redirect_uri=..."
}
```

---

### `GET /auth/info`

서버 인증 설정을 반환합니다 (registration mode, OIDC 가용 여부).

**응답** `200 OK`

```json
{
  "registration_mode": "open",
  "oidc_enabled": true,
  "server_version": "0.2.0"
}
```

---

## Device Code Flow 엔드포인트

Device code flow는 CLI 도구가 명령줄에 인증 정보를 전달하지 않고 브라우저를 통해 인증할 수 있게 합니다.

### `POST /device/code`

CLI 인증을 위한 device code를 요청합니다.

**응답** `200 OK`

```json
{
  "device_code": "550e8400-e29b-41d4-a716-446655440000",
  "user_code": "WDJB-MJHT",
  "verification_url": "https://sync.example.com/login/device",
  "expires_in": 300,
  "interval": 5
}
```

---

### `POST /device/token`

Device code 완료를 위해 폴링합니다. CLI는 지정된 `interval`마다 이 엔드포인트를 폴링합니다.

**요청 본문**

```json
{
  "device_code": "550e8400-e29b-41d4-a716-446655440000",
  "device_key": "optional-stable-uuid",
  "device_name": "optional-hostname"
}
```

| 필드 | 타입 | 필수 | 설명 |
|------|------|------|------|
| `device_code` | string | 필수 | `/device/code`가 반환한 코드 |
| `device_key` | string | - | 클라이언트의 안정적인 UUID. 제공 시 승인 시점에 디바이스로 등록됨 |
| `device_name` | string | - | 사람이 읽을 수 있는 디바이스 이름 (64자로 절단) |

**응답** `200 OK` (인증 완료)

```json
{
  "access_token": "eyJhbGciOi...",
  "refresh_token": "eyJhbGciOi...",
  "token_type": "Bearer",
  "expires_in": 3600
}
```

**에러**

| 상태 | 본문 | 설명 |
|------|------|------|
| `400` | `{ "error": "authorization_pending" }` | 사용자가 아직 승인하지 않음, 계속 폴링 |
| `400` | `{ "error": "slow_down", "interval": 10 }` | 클라이언트가 5초보다 빠르게 폴링 중 |
| `400` | `{ "error": "expired_token" }` | 알 수 없거나 이미 소비된 device code |
| `410` | `{ "error": "expired_token" }` | Device code 만료 |

---

### `POST /device/approve`

대기 중인 device code를 승인합니다. 로그인된 세션에서 사용자가 `user_code`를 제출한 뒤 브라우저가 호출합니다. JWT 필수.

**요청 본문**

```json
{
  "user_code": "WDJB-MJHT"
}
```

**응답** `204 No Content`.

**에러**

| 상태 | 메시지 | 설명 |
|------|--------|------|
| `404` | `invalid or expired code` | 알 수 없는 user_code |
| `409` | `code already approved` | 이미 소비된 코드 |
| `410` | `code expired` | 코드 만료 |

---

## OIDC 엔드포인트

OIDC가 설정되어 있을 때만 사용 가능합니다 (설정의 `oidc_*` 필드가 모두 설정됨).

### `GET /auth/oidc/authorize`

OIDC 로그인 플로우를 시작합니다. 사용자를 ID 프로바이더로 리다이렉트합니다.

**쿼리 파라미터**

| 파라미터 | 필수 | 설명 |
|----------|------|------|
| `redirect_uri` | - | 인증 후 클라이언트 리다이렉트 URI (CLI 플로우: localhost만 허용) |

**응답** `307 Temporary Redirect` — ID 프로바이더의 인가 엔드포인트로 리다이렉트.

---

### `GET /auth/callback`

OIDC 콜백 핸들러. 인가 코드를 토큰으로 교환하고 사용자를 찾거나 생성합니다.

**쿼리 파라미터**

| 파라미터 | 설명 |
|----------|------|
| `code` | ID 프로바이더의 인가 코드 |
| `state` | CSRF 상태 토큰 |
| `error` | ID 프로바이더의 에러 (선택) |

**응답**
- **CLI 플로우** (localhost `redirect_uri`): `307 Redirect` → `redirect_uri?access_token=...&refresh_token=...&token_type=Bearer&expires_in=...`
- **브라우저 플로우** (`redirect_uri` 없음): `307 Redirect` → `/admin#access_token=...&refresh_token=...&expires_in=...`

---

## 쿼리 (JWT 필수)

쿼리는 EventStore에서 직접 제공됩니다. 로컬 toki 데몬의 REPORT 프로토콜과 동일한 인터페이스 — toki 가상 쿼리(`usage{}`, `events{}`, `cost{}`)와 데몬의 JSON 출력 형식을 사용합니다.

### `GET /api/v1/toki/query`

instant(스탯)와 range(차트) 쿼리를 모두 처리하는 단일 엔드포인트. `step`이 주어지면 결과가 버킷팅되고, 없으면 전체 `[start, end)` 범위에 대한 단일 집계 결과가 반환됩니다.

**쿼리 파라미터**

| 파라미터 | 필수 | 설명 |
|----------|------|------|
| `query` | 필수 | Toki 가상 쿼리: `usage{}`, `events{}`, `cost{}`. `by (model)` 또는 `by (project)`로 그룹핑 |
| `start` | - | epoch 초, `YYYYMMDD`, `YYYYMMDDhhmmss` 형식. 기본값 `0` |
| `end` | - | `start`와 같은 형식. 기본값은 현재 시각 |
| `step` | - | 버킷 크기 (예: `3600`, `1h`, `1d`, `1w`). instant 쿼리는 생략 |
| `scope` | - | `self`(기본), `team:<team_id>`, `all`. 서버의 `max_query_scope`에 의해 제한 |
| `tz` | - | 버킷 포매팅용 IANA 타임존 (예: `Asia/Seoul`). 기본 UTC |
| `start_of_week` | - | `step=1w`일 때 주 시작 요일 (`mon`-`sun`). 기본 `mon` |

range 쿼리는 요청당 2000개 버킷으로 상한이 걸립니다 — 해당 범위에서 이를 초과하는 step은 서버가 거부합니다.

**응답** `200 OK`

```json
{
  "providers": {
    "claude_code": [
      {
        "period": "2026-03-28T00:00:00|claude-opus-4-6",
        "usage_per_models": [
          {
            "model": "claude-opus-4-6",
            "input_tokens": 12345,
            "output_tokens": 6789,
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 0,
            "total_tokens": 19134,
            "events": 42,
            "cost_usd": 0.18
          }
        ]
      }
    ]
  }
}
```

`period`는 `<ISO 타임스탬프>|<그룹 키>`입니다. Codex 프로바이더 항목은 캐시 필드 자리에 `cached_input_tokens`와 `reasoning_output_tokens`를 사용합니다. `cost_usd`는 `cost{}` 쿼리이거나 모델에 매칭되는 pricing 항목이 있을 때만 포함됩니다.

**에러**

| 상태 | 설명 |
|------|------|
| `400` | 잘못된 시간 형식, 잘못된 scope, 범위 대비 step이 너무 작거나 step > range |
| `403` | 서버가 해당 scope를 허용하지 않거나(`max_query_scope`) 팀 멤버가 아님 |
| `502` | EventStore 백엔드 사용 불가 |

---

## 사용자 셀프서비스 (JWT 필수)

### `GET /me/devices`

인증된 사용자의 모든 디바이스 목록을 반환합니다.

**응답** `200 OK`

```json
[
  {
    "device_id": "550e8400-e29b-...",
    "device_name": "macbook-pro",
    "last_seen": "2026-03-28T10:30:00Z"
  }
]
```

---

### `DELETE /me/devices/:device_id`

인증된 사용자의 디바이스를 제거합니다.

**응답** `200 OK`

```json
{ "deleted": true }
```

---

### `PATCH /me/devices/:device_id/name`

디바이스 이름을 변경합니다.

**요청 본문**

```json
{ "name": "work-laptop" }
```

**응답** `200 OK`

```json
{ "updated": true }
```

---

### `PATCH /me/password`

인증된 사용자의 비밀번호를 변경합니다.

**요청 본문**

```json
{
  "current_password": "old-password",
  "new_password": "new-strong-password"
}
```

**응답** `200 OK`

```json
{ "updated": true }
```

---

### `GET /me/teams`

인증된 사용자의 팀 멤버십 목록을 반환합니다.

**응답** `200 OK`

```json
[
  {
    "team_id": "team-uuid",
    "team_name": "engineering"
  }
]
```

---

## 관리자 엔드포인트 (JWT 필수, admin 역할)

모든 관리자 엔드포인트는 `admin` 역할을 가진 사용자의 JWT가 필요합니다.

### 설정

| 메서드 | 경로 | 설명 |
|--------|------|------|
| `GET` | `/admin/settings` | 현재 서버 설정 조회 (registration_mode, OIDC 필드, max_query_scope) |
| `PUT` | `/admin/settings/:key` | 키 단위로 설정 하나 변경 |

허용된 `:key` 값: `registration_mode`, `oidc_issuer`, `oidc_client_id`, `oidc_client_secret`, `oidc_redirect_uri`, `max_query_scope`.

#### `PUT /admin/settings/:key`

**요청 본문**

```json
{ "value": "approval" }
```

**응답** `204 No Content`. 알 수 없는 키이거나 검증에 실패하면 `422` (`registration_mode`는 `open|approval|closed`, `max_query_scope`는 `self|team|all`).

---

### 대기 중인 사용자

| 메서드 | 경로 | 설명 |
|--------|------|------|
| `GET` | `/admin/pending` | 승인 대기 중인 사용자 목록 (`registration_mode = "approval"` 시) |
| `POST` | `/admin/pending/:id/approve` | 대기 중인 등록 승인 |
| `POST` | `/admin/pending/:id/reject` | 대기 중인 등록 거부 |

---

### 서버 정보

| 메서드 | 경로 | 설명 |
|--------|------|------|
| `GET` | `/admin/server-info` | 서버 버전, 가동 시간, 연결된 디바이스 수, 데이터베이스 통계 |

---

### 역할 관리

| 메서드 | 경로 | 설명 |
|--------|------|------|
| `PATCH` | `/admin/users/:user_id/role` | 사용자 역할 변경 (`"admin"` 또는 `"user"`) |

---

### 사용자

| 메서드 | 경로 | 설명 |
|--------|------|------|
| `GET` | `/admin/users` | 전체 사용자 목록 |
| `POST` | `/admin/users` | 사용자 생성 |
| `DELETE` | `/admin/users/:user_id` | 사용자 삭제 |
| `PATCH` | `/admin/users/:user_id/password` | 사용자 비밀번호 변경 |
| `PATCH` | `/admin/users/:user_id/active` | 사용자 활성/비활성 전환 (`{ "active": bool }`) |

#### `POST /admin/users`

**요청 본문**

```json
{
  "username": "newuser",
  "password": "strong-password"
}
```

**응답** `201 Created`

```json
{
  "id": "550e8400-e29b-...",
  "username": "newuser"
}
```

#### `PATCH /admin/users/:user_id/password`

**요청 본문**

```json
{ "password": "new-password" }
```

### 디바이스

| 메서드 | 경로 | 설명 |
|--------|------|------|
| `GET` | `/admin/devices` | 전체 사용자의 모든 디바이스 목록 |
| `DELETE` | `/admin/devices/:device_id` | 디바이스 삭제 |

### 팀

| 메서드 | 경로 | 설명 |
|--------|------|------|
| `GET` | `/admin/teams` | 전체 팀 목록 |
| `POST` | `/admin/teams` | 팀 생성 |
| `DELETE` | `/admin/teams/:team_id` | 팀 삭제 |
| `GET` | `/admin/teams/:team_id/members` | 팀 멤버 목록 |
| `POST` | `/admin/teams/:team_id/members` | 팀 멤버 추가 |
| `DELETE` | `/admin/teams/:team_id/members/:user_id` | 팀 멤버 제거 |

#### `POST /admin/teams`

**요청 본문**

```json
{ "name": "engineering" }
```

#### `POST /admin/teams/:team_id/members`

**요청 본문**

```json
{ "user_id": "user-uuid" }
```

---

## 대시보드

| 메서드 | 경로 | 설명 |
|--------|------|------|
| `GET` | `/` | `/admin`으로 리다이렉트 |
| `GET` | `/admin` | 관리자 대시보드 (HTML/JS SPA) |
| `GET` | `/login` | 로그인 페이지 (HTML) |

대시보드는 브라우저 `localStorage`에 저장된 JWT로 인증합니다. OIDC 로그인 후에는 URL 프래그먼트(`#access_token=...`)를 통해 토큰이 전달됩니다.

---

## TCP 동기화 프로토콜 레퍼런스 (포트 9090)

포트 9090은 HTTP가 아닌 커스텀 바이너리 프로토콜(bincode 직렬화)을 사용합니다. 프로토콜은 `toki-sync-protocol` crate에 구현되어 있으며 직접 사용하기 위한 것이 아닙니다 — toki CLI(`toki settings sync enable`)로 연결하세요.

| 프레임 필드 | 크기 | 의미 |
|---|---|---|
| 메시지 타입 | 4바이트 (u32 LE) | 프레임 종류 (`AUTH`, `SYNC_BATCH` 등) |
| 페이로드 길이 | 4바이트 (u32 LE) | 페이로드 바이트 수 |
| 페이로드 | N바이트 | bincode로 인코딩된 메시지, 필요 시 zstd 압축 |

전체 메시지 타입 표, 핸드셰이크 시퀀스, 설계 근거는 [DESIGN.ko.md — Sync Protocol](DESIGN.ko.md#sync-protocol)을 참고하세요.
