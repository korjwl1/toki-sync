# 문제 해결

## Caddy가 TLS 인증서를 받지 못함

- DuckDNS 서브도메인이 올바른 IP를 가리키는지 확인: [https://www.duckdns.org](https://www.duckdns.org)에서 확인
- `.env`의 `DUCKDNS_TOKEN`이 올바른지 확인
- Caddy 로그 확인: `docker logs toki-caddy`
- 방화벽에서 포트 443과 9090이 차단되지 않았는지 확인
- Let's Encrypt에는 비율 제한이 있습니다 — 초과한 경우 잠시 기다린 후 재시도

---

## `toki settings sync enable`이 타임아웃됨

- 서버가 실행 중인지 확인: `docker compose ps`
- 포트 9090에 접근 가능한지 확인: `nc -zv yourserver.duckdns.org 9090`
- 서버의 방화벽 규칙 확인
- toki-sync-server 로그 확인: `docker logs toki-sync-server`

---

## "connection refused" 또는 "certificate error"

- 자체 서명 TLS (시나리오 C)의 경우 `--insecure` 플래그 사용:
  ```bash
  toki settings sync enable --server <ip> --insecure
  ```
- 도메인 기반 TLS (시나리오 A)의 경우 DNS가 전파되었는지 확인: `dig myserver.duckdns.org`
- `.env`의 `TOKI_EXTERNAL_URL`이 실제 도메인/IP와 일치하는지 확인

---

## 이벤트 스토어 문제

**Fjall (기본)**:
- Fjall은 내장되어 있습니다 -- 별도 컨테이너를 확인할 필요 없음. toki-sync-server 로그를 확인하세요: `docker logs toki-sync-server`
- `toki-data` 볼륨에 충분한 디스크 공간이 있는지 확인
- 데이터가 손상된 것으로 보이면 서버를 중지하고 `/data/events.fjall`을 삭제한 후 재시작하세요. 클라이언트가 전체 재동기화를 수행합니다.

**ClickHouse (선택)**:
- 로그 확인: `docker logs toki-clickhouse`
- 헬스 체크: `docker exec toki-clickhouse wget -qO- http://localhost:8123/ping`
- `clickhouse-data` 볼륨에 충분한 디스크 공간이 있는지 확인

---

## 대시보드에 데이터가 표시되지 않음

- 연결된 디바이스가 있는지 확인: `toki settings sync devices`
- 클라이언트에서 toki 데몬이 실행 중인지 확인: `toki daemon status`
- 클라이언트에서 동기화 상태 확인: `toki settings sync status`
- 서버 로그에서 에러 확인: `docker logs toki-sync-server`

---

## 로그인 시 "invalid credentials"

- 비밀번호가 `.env`의 `TOKI_ADMIN_PASSWORD`와 일치하는지 확인
- 관리자 계정은 첫 서버 시작 시 생성됩니다. 첫 시작 후에 `.env`에서 비밀번호를 변경해도 자동으로 업데이트되지 않습니다. API나 대시보드를 통해 변경하세요.

---

## 동기화 재연결 문제

- toki 데몬은 연결이 끊기면 지수 백오프 (2초에서 300초 상한)를 사용합니다
- 클라이언트 측 동기화 상태 확인: `toki settings sync status`
- toki 데몬 재시작: `toki daemon restart`
- 서버 로그에서 인증 에러 확인: `docker logs toki-sync-server`
