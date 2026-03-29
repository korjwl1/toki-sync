# Custom Dashboard Guide

toki-sync API를 사용하여 토큰 사용량 데이터를 시각화하는 커스텀 대시보드를 구축하는 방법을 설명합니다.

## 개요

toki-sync는 JWT 인증과 스코프 기반 접근 제어가 포함된 PromQL 프록시를 제공합니다. 모든 쿼리는 toki-sync를 통해 전달되며, VictoriaMetrics로 전달하기 전에 PromQL 표현식에 레이블 필터를 주입하여 사용자 수준의 데이터 격리를 시행합니다.

대시보드 구축에는 두 가지 방식이 있습니다:

- **Tier 1: 직접 연결** -- 프론트엔드가 toki-sync API와 직접 통신
- **Tier 2: 커스텀 백엔드** -- 자체 백엔드가 프론트엔드와 toki-sync/VictoriaMetrics 사이를 중개

## Tier 1: 직접 연결

아키텍처: `프론트엔드 -> toki-sync API`

가장 간단한 방식입니다. 프론트엔드가 toki-sync에 인증하고 데이터를 직접 조회합니다.

### 인증

1. 자격 증명을 POST하여 JWT를 획득합니다:

```
POST /login
Content-Type: application/json

{"username": "alice", "password": "..."}
```

응답에 `access_token`과 `refresh_token`이 포함됩니다.

2. 이후 모든 요청에 JWT를 포함합니다:

```
Authorization: Bearer <access_token>
```

3. 만료된 토큰 갱신:

```
POST /token/refresh
Content-Type: application/json

{"refresh_token": "<refresh_token>"}
```

### 쿼리 엔드포인트

**즉시 쿼리:**

```
GET /api/v1/query?query=<promql>&time=<unix_ts>&scope=<scope>
```

**범위 쿼리:**

```
GET /api/v1/query_range?query=<promql>&start=<unix_ts>&end=<unix_ts>&step=<duration>&scope=<scope>
```

### Scope 파라미터

`scope` 파라미터는 누구의 데이터가 보이는지를 제어합니다:

| Scope | 설명 | 요구사항 |
|-------|------|----------|
| `self` (기본값) | 인증된 사용자 본인의 데이터만 | 항상 허용 |
| `team:<TEAM_ID>` | 지정된 팀의 모든 멤버 데이터 | `max_query_scope`가 `team` 또는 `all`이어야 하며, 사용자가 해당 팀의 멤버여야 함 |
| `all` | 모든 사용자 (조직 전체) | `max_query_scope`가 `all`이어야 함 |

관리자(Admin) 사용자는 모든 스코프 제한을 우회하며, `scope` 파라미터나 서버 설정에 관계없이 항상 모든 데이터를 볼 수 있습니다.

### PromQL 쿼리 예시

**내 총 토큰 사용량:**

```
GET /api/v1/query?query=sum(toki_tokens_total)
```

**모델별 내 사용량 (시계열):**

```
GET /api/v1/query_range?query=sum by (model)(increase(toki_tokens_total[1h]))&start=1711584000&end=1711670400&step=3600
```

**팀 리더보드 (사용자별 토큰):**

```
GET /api/v1/query?query=sum by (user)(toki_tokens_total)&scope=team:TEAM_ID
```

**조직 전체 사용량:**

```
GET /api/v1/query?query=sum(toki_tokens_total)&scope=all
```

**시간별 사용량 (시간당 증가율):**

```
GET /api/v1/query_range?query=sum(increase(toki_tokens_total[1h]))&start=...&end=...&step=3600
```

**프로바이더별 사용량:**

```
GET /api/v1/query_range?query=sum by (provider)(increase(toki_tokens_total[1h]))&start=...&end=...&step=3600
```

## Tier 2: 커스텀 백엔드

아키텍처: `프론트엔드 -> 자체 백엔드 -> toki-sync (사용자/팀 정보) + VictoriaMetrics (데이터)`

`self`/`team`/`all` 이상의 세밀한 접근 제어가 필요하거나, toki-sync 데이터를 다른 데이터 소스와 결합하려는 경우 이 방식을 사용합니다.

### 동작 방식

1. 자체 백엔드가 toki-sync JWT를 통해 사용자를 인증합니다 (동일한 JWT 시크릿으로 토큰 검증)
2. 자체 백엔드가 toki-sync API에서 사용자/팀 정보를 가져옵니다:
   - `GET /me/teams` -- 사용자의 팀 목록
   - `GET /admin/users` -- 모든 사용자 목록 (관리자 전용)
   - `GET /admin/teams/:team_id/members` -- 팀 멤버 목록 (관리자 전용)
3. 자체 백엔드가 내부 네트워크에서 VictoriaMetrics에 직접 쿼리합니다 (VM에는 JWT 불필요)
4. 자체 백엔드가 프론트엔드에 데이터를 반환하기 전에 자체 권한 로직을 적용합니다

### VictoriaMetrics 직접 쿼리

VictoriaMetrics는 동일한 PromQL 엔드포인트를 지원합니다:

```
GET http://victoriametrics:8428/api/v1/query_range?query=...&start=...&end=...&step=...
```

VM에 직접 쿼리할 때는 접근 제어를 시행하기 위해 `user="..."` 또는 `user=~"..."` 레이블 필터를 직접 주입해야 합니다.

## 사용 가능한 레이블

모든 toki 메트릭에는 다음 레이블이 포함됩니다:

| 레이블 | 설명 | 예시 값 |
|--------|------|---------|
| `user` | 사용자 ID (UUID) | `550e8400-e29b-41d4-a716-446655440000` |
| `device` | 디바이스 ID (UUID) | `6ba7b810-9dad-11d1-80b4-00c04fd430c8` |
| `model` | AI 모델 이름 | `claude-sonnet-4-20250514`, `gpt-4o` |
| `provider` | 프로바이더/도구 이름 | `claude_code`, `cursor`, `chatgpt` |
| `session` | 세션 식별자 | `session-abc123` |
| `project` | 프로젝트 이름 | `my-app` |
| `type` | 토큰 유형 | `input`, `output`, `cache_create`, `cache_read` |

## 토큰 유형

`type` 레이블은 서로 다른 토큰 범주를 구분합니다:

- `input` -- 모델에 전송된 토큰 (프롬프트 토큰)
- `output` -- 모델이 생성한 토큰 (완성 토큰)
- `cache_create` -- 프롬프트 캐시에 기록된 토큰
- `cache_read` -- 프롬프트 캐시에서 읽은 토큰

**예시: 비용 관련 토큰만 (input + output):**

```promql
sum(toki_tokens_total{type=~"input|output"})
```

## 중요 사항

- `scope=all`은 서버 관리자가 서버 설정에서 `max_query_scope`를 `all`로 설정해야 합니다
- `scope=team:ID`는 `max_query_scope`가 `team` 또는 `all`이어야 합니다
- 프롬프트나 응답은 절대 저장되지 않습니다 -- 토큰 수와 메타데이터만 저장됩니다
- 관리자 사용자는 스코프 설정에 관계없이 항상 전체 접근 권한을 가집니다
- 팀 전용 엔드포인트 `GET /api/v1/teams/:team_id/query_range`는 `scope=team:ID`의 대안으로 계속 사용 가능합니다
