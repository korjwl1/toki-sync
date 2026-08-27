# 커스텀 대시보드

toki-sync의 인증된 가상 쿼리 엔드포인트를 이용해 클라이언트를 구축합니다. 서버는
`/api/v1/query`, `/api/v1/query_range`, PromQL 프록시 또는 VictoriaMetrics 설정을
제공하지 않습니다. 현재 구현된 사용량 쿼리 경로는
`GET /api/v1/toki/query` 하나입니다.

## 아키텍처

```text
브라우저 또는 커스텀 백엔드
    |  Authorization: Bearer <access token>
    v
toki-sync /api/v1/toki/query
    |
    +-- Fjall 또는 ClickHouse EventStore
```

브라우저 전용 UI는 `POST /login`으로 토큰을 받고 refresh token이 로그나 URL에
들어가지 않게 하세요. 여러 데이터 소스를 결합하거나 프런트엔드에서 인증 정보를
분리하려면 커스텀 백엔드가 더 안전합니다. 사용자의 access token을 toki-sync에
전달할 수 있지만 사용자 대신 더 넓은 scope를 만들어내면 안 됩니다.

## 인증

```http
POST /login
Content-Type: application/json

{"username":"alice","password":"..."}
```

반환된 access token을 사용합니다.

```http
Authorization: Bearer <access_token>
```

만료된 토큰 쌍은 `POST /token/refresh`로 회전합니다. 정확한 요청/응답 계약은
[API.ko.md](API.ko.md)를 참고하세요.

## 쿼리

instant 집계는 `step`을 생략합니다.

```http
GET /api/v1/toki/query?query=sum%20by%20(model)(usage)&start=1787788800&end=1787875200&scope=self
```

차트는 `step`을 지정합니다.

```http
GET /api/v1/toki/query?query=sum%20by%20(model)(usage)&start=20260801&end=20260827&step=1d&tz=Asia%2FSeoul&scope=self
```

지원하는 쿼리 구성요소는 다음과 같습니다.

- metric: `usage`(또는 이전 이름 `toki_tokens_total`), `cost`, `events`
- 선택적 `sum(...)`, `increase(...)` wrapper
- `provider="..."` 동등 필터 하나
- `model`, `project`, `device_id` 중 한 grouping 차원(`type`은 함께 쓸 수 있지만
  토큰 종류는 필드로 출력됨)
- bare `windows` (`scope=self`만 지원)

쿼리 안 range selector는 문법만 검사하며 실제 스캔과 버킷은 HTTP의 `start`, `end`,
`step`이 결정합니다. 지원하지 않는 PromQL 함수, 임의 라벨, 정규식 matcher,
`offset`, `sessions`, `projects`는 `400`을 반환합니다.

## Scope

| Scope | 데이터 | 조건 |
|---|---|---|
| `self` | 인증된 사용자의 이벤트 | 항상 가능 |
| `team:<team_id>` | 한 팀의 현재 멤버 | 비관리자는 팀 멤버여야 하며 서버 최대 scope가 `team` 또는 `all` |
| `all` | 모든 사용자 | 비관리자는 서버 최대 scope가 `all` |

관리자는 설정된 최대 scope를 우회하지만 요청한 scope는 계속 결과를 좁힙니다. 관리자가
`self`를 요청하면 본인 데이터만 받습니다.

## 응답 처리

집계는 toki의 provider-grouped JSON 형태입니다.

```json
{
  "providers": {
    "claude_code": [
      {
        "period": "2026-08-01T00:00:00|claude-opus-4-6",
        "usage_per_models": [
          {
            "model": "claude-opus-4-6",
            "input_tokens": 100,
            "output_tokens": 20,
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 50,
            "total_tokens": 170,
            "events": 2,
            "cost_usd": 0.001
          }
        ]
      }
    ]
  }
}
```

`model`이라는 필드가 항상 모델을 담는다고 가정하지 마세요. project 또는 device로
그룹화하면 로컬 클라이언트 호환성을 위해 선택한 그룹 키가 그 필드에 들어갑니다.
Codex 항목은 Claude 캐시 필드 대신 `cached_input_tokens`와
`reasoning_output_tokens`를 사용합니다.

## 반드시 표시해야 하는 제한

- 시계열 쿼리는 2,000개 시간 버킷으로 제한됩니다.
- 서버는 요청당 입력 이벤트를 최대 200,000개 읽으며 cursor가 없습니다.
- bare raw `events`는 최상위 `"truncated": true`를 반환할 수 있습니다.
- 집계는 현재 200,000개 입력 잘림을 전파하지 않아 넓은 집계가 완전한 것처럼 보일
  수 있습니다. 제한된 시간 범위를 사용하세요.
- 집계가 서로 다른 `(bucket, group)` 조합 50,000개를 넘으면
  `"truncated": true`를 반환합니다.
- ClickHouse raw 스캔은 이벤트 상한 전에 어댑터의 10 MiB 버퍼 응답 제한으로
  `502`가 날 수 있습니다. 더 좁은 범위로 재시도하세요.
- RFC 3339와 dashed date는 원격에서 허용하지 않습니다. epoch 초, 13자리 epoch
  밀리초, `YYYYMMDD`, `YYYYMMDDhhmmss`를 사용하세요.
- 서버 범위는 `[start, end)`입니다. 단 날짜 전용 end는 해당 날짜를 포함하도록 다음
  현지 자정으로 올립니다.

넓은 집계에서 `truncated`가 없다는 것을 모든 이벤트가 계산됐다는 증거로 취급하지
마세요.

## 보안 참고

- 토큰 수와 메타데이터만 저장하며 prompt와 response는 저장하지 않습니다.
- 신뢰하지 않는 프런트엔드에서 EventStore DB를 직접 호출하지 마세요.
- toki-sync 평문 포트를 인터넷에 노출하지 마세요. 신뢰하는 리버스 프록시에서 HTTP
  9091과 TCP 9090의 TLS를 모두 종단하세요.
- 내장 `/admin` 페이지는 관리 콘솔이며 사용량 차트 UI가 아닙니다.

전체 파라미터와 응답 형태는 [API.ko.md](API.ko.md)를 참고하세요.
