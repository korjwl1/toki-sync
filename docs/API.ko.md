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
  "oidc_enabled": true
}
```

공개 auth-info 응답은 서버 버전을 노출하지 않습니다. 관리자는
`GET /admin/server-info`에서 Cargo 패키지 버전을 확인할 수 있습니다.

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

쿼리는 EventStore에서 직접 제공됩니다. 일반 PromQL 엔드포인트가 아니라 toki 가상
쿼리 언어의 의도적으로 제한된 부분집합입니다.

### `GET /api/v1/toki/query`

instant(스탯)와 range(차트) 쿼리를 모두 처리하는 단일 엔드포인트. `step`이 주어지면 결과가 버킷팅되고, 없으면 전체 `[start, end)` 범위에 대한 단일 집계 결과가 반환됩니다.

**쿼리 파라미터**

| 파라미터 | 필수 | 설명 |
|----------|------|------|
| `query` | 필수 | `usage`/`toki_tokens_total`, `cost`, `events`, 또는 bare `windows`; 선택적 `sum`, `increase`, provider 동등 필터, `model`/`project`/`device_id` 중 하나로 그룹핑 |
| `start` | - | epoch 초, 13자리 epoch 밀리초, `YYYYMMDD`, `YYYYMMDDhhmmss`. 기본값 `0` |
| `end` | - | `start`와 같은 형식. 기본값은 현재 시각 |
| `step` | - | 버킷 크기 (예: `3600`, `1h`, `1d`, `1w`). instant 쿼리는 생략 |
| `scope` | - | `self`(기본), `team:<team_id>`, `all`. 서버의 `max_query_scope`에 의해 제한 |
| `tz` | - | 버킷 포매팅용 IANA 타임존 (예: `Asia/Seoul`). 기본 UTC |
| `start_of_week` | - | `step=1w`일 때 주 시작 요일 (`mon`-`sun`). 기본 `mon` |
| `no_cost` | - | boolean 쿼리 값. true이면 비용 계산 생략 |

서버 범위는 반열린 `[start, end)`입니다. 날짜 전용 `end=YYYYMMDD`는 다음 현지
자정으로 변환되어 해당 날짜 전체를 포함합니다. 숫자 초/밀리초와
`YYYYMMDDhhmmss` end는 정확한 배타 경계입니다. RFC 3339와 dashed date는 현재
`400`으로 거부됩니다. 원격 API는 로컬 toki가 이해하는 모든 시간 표기를 아직
지원하지 않습니다.

range 쿼리는 2,000개 시간 버킷으로 제한됩니다. 라벨 필터는
`provider="value"`의 `=` 연산자만 허용합니다. 정규식/부정 matcher, `offset`,
`avg`, `count`, `sessions`, `projects`, 임의 PromQL은 근사하지 않고 거부합니다.
bare `windows`는 `scope=self`만 지원합니다.

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

`period`는 `<ISO 타임스탬프>|<그룹 키>`입니다. Codex 항목은 Claude 캐시 필드 대신
`cached_input_tokens`와 `reasoning_output_tokens`를 사용합니다. `no_cost=true`가
아니면 usage/cost 버킷과 raw 이벤트는 모델 가격을 알 때 `cost_usd`를 포함합니다.
집계 `events`는 개수이므로 비용을 포함하지 않습니다.

### 쿼리 상한과 부분 결과

- 요청 하나는 최대 200,000개 이벤트를 읽습니다. pagination이 아닌 하드 상한입니다.
- bare raw `events`는 이벤트 상한에 닿으면 최상위에 `"truncated": true`를
  추가합니다. API 직접 사용자는 이를 검사해야 하며 현재 toki CLI 어댑터는 이
  최상위 플래그를 버립니다.
- 집계 쿼리도 입력 200,000개 상한을 적용하지만 현재 응답은 입력 스캔이 잘렸다는
  사실을 표시하지 않습니다. 합계가 부분값일 수 있으므로 pagination 또는 명시적
  전파가 구현되기 전까지 시간 범위를 좁히세요.
- 집계는 별도로 `(bucket, group)` 조합을 50,000개로 제한하며 이 상한에 닿으면
  `"truncated": true`를 반환합니다.
- ClickHouse 어댑터는 `JSONEachRow` 결과를 ureq의 10 MiB 문자열 reader로
  버퍼링합니다. 넓은 raw 쿼리는 200,000개 전에 `502`로 실패할 수 있습니다. 현재
  cursor 또는 응답 크기에 안전한 streaming 경로가 없습니다.
- Fjall team 스캔은 멤버를 순서대로 방문합니다. 이벤트 상한에 닿으면 앞선 멤버가
  허용량을 모두 쓸 수 있어 잘린 팀 결과는 전역 시간순 표본이 아닙니다.

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
{
  "devices": [
    {
      "id": "550e8400-e29b-...",
      "name": "macbook-pro",
      "device_key": "stable-client-key",
      "last_seen_at": 1774693800
    }
  ]
}
```

---

### `DELETE /me/devices/:device_id`

인증된 사용자의 디바이스를 제거합니다.

**응답** `204 No Content`.

---

### `PATCH /me/devices/:device_id/name`

디바이스 이름을 변경합니다.

**요청 본문**

```json
{ "name": "work-laptop" }
```

**응답** `204 No Content`.

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

**응답** `204 No Content`. 사용자의 모든 refresh token이 폐기됩니다.

---

### `GET /me/teams`

인증된 사용자의 팀 멤버십 목록을 반환합니다.

**응답** `200 OK`

```json
{
  "teams": [
    {
      "team_id": "team-uuid",
      "team_name": "engineering",
      "role": "member"
    }
  ]
}
```

---

## 모니터 설정 동기화 (JWT 필수)

toki_monitor의 설정과 대시보드 정의를 위한 **선택적(opt-in)** 채널입니다. TCP 동기화
프로토콜과 의도적으로 분리되어 있습니다. 그 프로토콜은 toki가 수집한 사용량 데이터만
운반하며, 모니터를 쓰지 않는 toki 사용자는 이 엔드포인트를 전혀 건드리지 않습니다.

페이로드는 **불투명(opaque)** 합니다. 서버는 받은 바이트를 그대로 저장하고 그대로
돌려줍니다. 대시보드를 파싱하지 않으므로, 모니터의 정의와 어긋나는 서버 측 정의가
생길 여지가 없습니다. 따라서 `value`는 JSON 객체가 아니라 **문자열**입니다. 클라이언트가
직접 직렬화한 문자열을 보내면 바이트 단위로 동일하게 되돌아옵니다.

사용 전에 `GET /api/v1/capabilities`에서 `monitor_settings_v1`을 확인하세요. 구버전
서버는 이 경로를 404로 응답하는데, 이는 저장된 항목이 없는 상태와 구분되지 않습니다.

**키**는 ASCII 영숫자와 `.`, `_`, `-`, `:` 로 이루어진 1~128자입니다
(예: `dashboard:main`, `prefs.theme`). 그 외는 `422`입니다.

**제한**

| 제한 | 값 | 초과 시 |
|------|-----|---------|
| 요청 본문 | 2 MiB | `413` (본문을 버퍼링하기 전에 거부) |
| `value` 하나 | 256 KiB | `413` |
| 사용자당 항목 수 | 512 | `507` |
| 사용자당 총 바이트 | 8 MiB | `507` |
| 사용자당 쓰기 | 60회 / 60초 | `429` (`Retry-After` 포함) |

읽기에는 제한이 없습니다. 백오프를 통보받은 클라이언트도 현재 상태는 읽을 수 있어야
하기 때문입니다. `PUT`과 `DELETE`는 모두 쓰기 예산을 소모합니다.

### `GET /me/monitor/index`

페이로드를 **제외한** 저장 목록과 잔여 용량을 반환합니다. 보유 중인 `version`과 비교해
변경된 항목만 내려받으면 됩니다.

**응답** `200 OK`

```json
{
  "delete_cas": true,
  "entries": [
    { "key": "dashboard:main", "version": 3, "updated_at": 1756200000, "size_bytes": 4210 }
  ],
  "quota": {
    "max_entries": 512,
    "max_value_bytes": 262144,
    "max_total_bytes": 8388608,
    "used_entries": 1,
    "used_bytes": 4210
  }
}
```

### `GET /me/monitor/settings`

저장된 모든 항목을 페이로드까지 포함해 반환합니다.

**응답** `200 OK`

```json
{
  "entries": [
    { "key": "dashboard:main", "value": "{\"panels\":[]}", "version": 3, "updated_at": 1756200000 }
  ]
}
```

### `GET /me/monitor/settings/:key`

항목 하나를 반환합니다.

**응답** `200 OK`

```json
{ "key": "dashboard:main", "value": "{\"panels\":[]}", "version": 3, "updated_at": 1756200000 }
```

해당 키가 없으면 `404`입니다.

### `PUT /me/monitor/settings/:key`

항목 하나를 저장하거나 교체합니다.

**요청**

```json
{ "value": "{\"panels\":[]}", "if_version": 3 }
```

`if_version`은 선택 항목입니다. 지정하면 저장된 버전이 일치할 때만 쓰기가 반영됩니다.
`0`은 "항목이 없어야 함"(생성 전용)을 뜻합니다.

**응답** `200 OK`

```json
{
  "key": "dashboard:main",
  "version": 4,
  "updated_at": 1756200100,
  "previous_version": 3,
  "created": false
}
```

**동시 쓰기.** 두 기기가 같은 항목을 편집하는 것은 오류가 아니라 정상적인 경우입니다.
규칙은 윈도우 병합과 동일하게 **서버 시각 기준 last-write-wins** 이지만, 클라이언트가
자신이 이겼는지 모른 채 남겨지는 일은 없습니다.

- `if_version`을 **지정**하고 졌다면 → `409 Conflict`, 아무것도 기록되지 않습니다.

  ```json
  {
    "error": "version conflict: the stored entry moved since if_version was read",
    "key": "dashboard:main",
    "current_version": 5,
    "current_updated_at": 1756200090
  }
  ```

  다시 조회해 병합한 뒤 `current_version`으로 재시도하세요.

- `if_version`을 **지정하지 않으면** → 쓰기는 항상 반영되지만, `previous_version`이
  무엇을 덮어썼는지 알려줍니다. 그 값이 내가 조회했던 버전과 다르면 그 사이에 다른
  기기가 기록했고 방금 그것을 덮어쓴 것입니다. 새 항목이면 `previous_version`은
  `null`, `created`는 `true`입니다.

`version`은 1에서 시작해 쓰기마다 증가합니다. 삭제 후 다시 만들면 1부터 시작하므로,
버전 1은 언제나 "새 항목"을 의미합니다.

**오류**

| 상태 | 상황 |
|------|------|
| `413 Payload Too Large` | `value`가 256 KiB 초과, 또는 요청 본문이 2 MiB 초과 |
| `422 Unprocessable Entity` | 허용되지 않는 형태의 키 |
| `429 Too Many Requests` | 쓰기 예산 소진. 본문에 `retry_after` 포함 |
| `507 Insufficient Storage` | 사용자당 용량 초과. 본문에 `quota`(`entries` \| `bytes`), `used`, `limit` 포함 |

### `DELETE /me/monitor/settings/:key`

항목 하나를 삭제합니다. 툼스톤이 아니라 행 자체가 제거됩니다.

선택 쿼리 파라미터 `if_version=<n>`으로 compare-and-swap 삭제를 수행합니다. 저장된
버전이 다르면 조건부 `PUT`과 같은 버전 상세 형태의 `409`를 반환합니다. 이전 서버에
사용하기 전 index의 `delete_cas` 플래그를 확인하세요.

**응답** `204 No Content`. 해당 키가 없으면 `404`입니다.

계정을 삭제하면 모니터 설정도 함께 삭제되므로, 방치된 계정이 행을 남기지 않습니다.

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
| `GET` | `/admin/server-info` | 가입/OIDC 상태, 관계형 스토리지 백엔드, Cargo 패키지 버전 |

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
| `GET` | `/admin` | 관리 콘솔 (HTML/JS SPA) |
| `GET` | `/login` | 로그인 페이지 (HTML) |

내장 페이지는 사용자, 디바이스, 팀, 가입, OIDC, 쿼리 scope를 관리합니다. 토큰 사용량
차트 대시보드는 아닙니다. 브라우저 `localStorage`의 JWT로 인증하며 OIDC 로그인 후
토큰은 URL 프래그먼트로 전달됩니다.

---

## 기능 탐색

공개 `GET /api/v1/capabilities`는 현재 다음을 반환합니다.

```json
{ "sync_windows_v1": true, "monitor_settings_v1": true }
```

클라이언트는 선택 protocol 메시지를 보내거나 monitor 설정 채널을 사용하기 전에 이를
확인해야 합니다. 이전 서버는 `404`를 반환합니다.

---

## TCP 동기화 프로토콜 레퍼런스 (포트 9090)

포트 9090은 HTTP가 아닌 커스텀 바이너리 프로토콜(bincode 직렬화)을 사용합니다.
protocol은 `toki-sync-protocol`에 구현되어 있으며 직접 사용하기 위한 것이 아닙니다.
현재 소스 브랜치는 로컬 patch를 통해 protocol 1.1.0 타입을 요구하지만 이 문서 갱신
시점의 원격 태그는 v1.0.0뿐입니다.

| 프레임 필드 | 크기 | 의미 |
|---|---|---|
| 메시지 타입 | 4바이트 (u32 LE) | 프레임 종류 (`AUTH`, `SYNC_BATCH` 등) |
| 페이로드 길이 | 4바이트 (u32 LE) | 페이로드 바이트 수 |
| 페이로드 | N바이트 | bincode로 인코딩된 메시지, 필요 시 zstd 압축 |

전체 메시지 타입 표, 핸드셰이크 시퀀스, 설계 근거는 [DESIGN.ko.md — Sync Protocol](DESIGN.ko.md#sync-protocol)을 참고하세요.
