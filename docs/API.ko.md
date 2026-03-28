# toki-sync HTTP API 레퍼런스

모든 HTTP 엔드포인트는 포트 9091에서 제공됩니다 (`[server].http_port`로 설정 가능).

## 인증

JWT 인증이 필요한 엔드포인트는 `Authorization` 헤더가 필요합니다:

```
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
| `username` | string | O | 계정 사용자명 |
| `password` | string | O | 계정 비밀번호 |
| `device_id` | string | X | 디바이스 식별자 (디바이스별 관리를 위해 리프레시 토큰에 포함) |

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

새 사용자 계정을 셀프 등록합니다. 설정에서 `allow_registration = true`일 때만 사용 가능합니다.

**요청 본문**

```json
{
  "username": "newuser",
  "password": "strong-password"
}
```

| 필드 | 타입 | 필수 | 제약 |
|------|------|------|------|
| `username` | string | O | 3-32자, 영숫자 + `_`, `-`, `.` |
| `password` | string | O | 8-128자 |

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
| `403` | `registration is disabled` | `allow_registration`이 `false` |
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
| `refresh_token` | string | O | 현재 리프레시 토큰 |
| `device_id` | string | X | 디바이스 식별자 |

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

## OIDC 엔드포인트

OIDC가 설정되어 있을 때만 사용 가능합니다 (설정의 `oidc_*` 필드가 모두 설정됨).

### `GET /auth/oidc/authorize`

OIDC 로그인 플로우를 시작합니다. 사용자를 ID 프로바이더로 리다이렉트합니다.

**쿼리 파라미터**

| 파라미터 | 필수 | 설명 |
|----------|------|------|
| `redirect_uri` | X | 인증 후 클라이언트 리다이렉트 URI (CLI 플로우: localhost만 허용) |

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
- **브라우저 플로우** (`redirect_uri` 없음): `307 Redirect` → `/dashboard#access_token=...&refresh_token=...&expires_in=...`

---

## PromQL 프록시 (JWT 필수)

PromQL 쿼리를 VictoriaMetrics에 프록시하며, 사용자별 label injection으로 데이터 격리를 보장합니다. 각 사용자는 자신의 데이터만 조회할 수 있습니다.

### `GET /api/v1/query`

즉시 PromQL 쿼리.

**쿼리 파라미터**

| 파라미터 | 필수 | 설명 |
|----------|------|------|
| `query` | O | PromQL 표현식 |
| `time` | X | 평가 타임스탬프 (RFC3339 또는 Unix 타임스탬프) |

**응답** `200 OK` — VictoriaMetrics 응답 형식:

```json
{
  "status": "success",
  "data": {
    "resultType": "vector",
    "result": [
      {
        "metric": { "__name__": "toki_tokens_total", "model": "claude-opus-4-6" },
        "value": [1711929600, "12345"]
      }
    ]
  }
}
```

---

### `GET /api/v1/query_range`

범위 PromQL 쿼리.

**쿼리 파라미터**

| 파라미터 | 필수 | 설명 |
|----------|------|------|
| `query` | O | PromQL 표현식 |
| `start` | O | 시작 타임스탬프 |
| `end` | O | 종료 타임스탬프 |
| `step` | O | 쿼리 해상도 단계 (예: `60s`, `5m`, `1h`) |

**응답** `200 OK` — `resultType: "matrix"`인 VictoriaMetrics 응답 형식.

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

## 팀 (JWT 필수)

### `GET /api/v1/teams/:team_id/query_range`

팀 멤버 전체에 대한 집계 PromQL 범위 쿼리. 서버가 팀의 모든 사용자에 대해 regex label matcher를 주입합니다.

**쿼리 파라미터** — `/api/v1/query_range`와 동일.

**응답** `200 OK` — VictoriaMetrics 응답 형식.

---

## 관리자 엔드포인트 (JWT 필수, admin 역할)

모든 관리자 엔드포인트는 `admin` 역할을 가진 사용자의 JWT가 필요합니다.

### 사용자

| 메서드 | 경로 | 설명 |
|--------|------|------|
| `GET` | `/admin/users` | 전체 사용자 목록 |
| `POST` | `/admin/users` | 사용자 생성 |
| `DELETE` | `/admin/users/:user_id` | 사용자 삭제 |
| `PATCH` | `/admin/users/:user_id/password` | 사용자 비밀번호 변경 |

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
| `GET` | `/` | `/dashboard`로 리다이렉트 |
| `GET` | `/dashboard` | 웹 대시보드 (HTML/JS SPA) |
| `GET` | `/login` | 로그인 페이지 (HTML) |

대시보드는 브라우저 `localStorage`에 저장된 JWT로 인증합니다. OIDC 로그인 후에는 URL 프래그먼트(`#access_token=...`)를 통해 토큰이 전달됩니다.

---

## TCP 동기화 프로토콜 (포트 9090)

TCP 포트는 HTTP가 **아닙니다**. toki 데몬 연결을 위한 커스텀 바이너리 프로토콜(bincode 직렬화)을 사용합니다:

1. 클라이언트가 TLS로 연결
2. 클라이언트가 `AuthRequest` 전송 (사용자명 + JWT 또는 비밀번호)
3. 서버가 `AuthResponse` 응답 (성공 + device_id)
4. 클라이언트가 `SyncBatch` 배치 전송 (이벤트, 100개 이상 시 zstd 압축)
5. 서버가 배치당 `SyncAck` 응답

이 프로토콜은 `toki-sync-protocol` 공유 crate에 구현되어 있으며 직접 사용하기 위한 것이 아닙니다. toki CLI(`toki sync enable`)로 연결하세요.
