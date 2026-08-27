# 문제 해결

먼저 `docker compose ps`와 `docker logs <container-name>`으로 container 상태와 오류를
확인하세요.

## 번들 Caddy 인증서를 신뢰하지 않음

현재 Caddyfile은 `TLS_MODE=internal`을 기본으로 사용합니다. `DUCKDNS_TOKEN`을
사용하거나 공개 Let's Encrypt 인증서를 발급하지 않습니다. 신뢰하는 LAN에서만
`--insecure`를 사용하거나 별도로 검증한 공개 TLS proxy 뒤에 두세요.
[DuckDNS 상태 가이드](deploy-caddy-duckdns.ko.md)를 참고하세요.

## `toki settings sync enable` 타임아웃

- 서버 상태: `docker compose ps`
- TCP 접근성: `nc -zv <server> 9090`
- 방화벽과 `docker logs toki-sync-server` 확인
- 직접 평문 포트에는 `--no-tls`, 번들 내부 CA에는 신뢰하는 LAN에서만 `--insecure`

## 이벤트 스토어 문제

### Fjall

- 별도 container는 없습니다. `docker logs toki-sync-server`와 `toki-data` 디스크
  공간을 확인하세요.
- 관계형 cursor를 둔 채 `/data/events.fjall`만 삭제하지 마세요. reconnect가 cursor를
  자동 reset하지 않습니다. 일관된 backup을 복원하거나 명시적 cursor reset/전체
  재동기화를 계획하세요.

### ClickHouse

- 로그: `docker logs toki-clickhouse`
- health: `docker exec toki-clickhouse wget -qO- http://localhost:8123/ping`
- `config/toki-sync.toml`의 `backend="clickhouse"`와 `clickhouse_url` 확인
- `system.tables.sorting_key`에서 `toki_events`가
  `(device_id, provider, msg_id)`인지 확인. 이전 키는 자동 migration되지 않습니다.
- 넓은 query는 adapter의 10 MiB 버퍼 응답 제한으로 `502`가 날 수 있습니다. 시간
  범위를 좁혀 재시도하세요.

## 원격 쿼리에 데이터가 없음

- `toki settings sync devices`, `toki daemon status`, `toki settings sync status` 확인
- `docker logs toki-sync-server`에서 sync/error 확인
- 내장 `/admin` 페이지에는 사용량 차트가 없습니다. `toki query --remote ...` 또는
  Toki Monitor를 사용하세요.

## 로그인 실패

`TOKI_ADMIN_PASSWORD`는 DB에 admin이 처음 생성될 때만 사용됩니다. 이후 `.env` 값을
바꿔도 저장된 hash는 바뀌지 않습니다. 관리 콘솔/API에서 비밀번호를 변경하세요.

## 반복 재연결

- client sync 상태와 server auth log를 확인하세요.
- daemon 재시작: `toki daemon restart`
- token이 만료/폐기됐으면 `sync disable --keep` 후 다시 enable하세요.
