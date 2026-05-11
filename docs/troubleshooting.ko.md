# 문제 해결

이 페이지는 증상을 아래 섹션으로 안내합니다. 어떤 문제든 첫 진단 명령은 `docker compose ps` (컨테이너 상태 확인)와 `docker logs <컨테이너 이름>` (에러 메시지 확인)입니다.

## 증상 매핑

| 증상 | 가능 원인 | 섹션 |
|---|---|---|
| Caddy가 시작되지 않음, HTTPS 안 됨 | DuckDNS 설정 오류, 443 포트 차단, Let's Encrypt rate limit | [Caddy가 TLS 인증서를 받지 못함](#caddy가-tls-인증서를-받지-못함) |
| `toki settings sync enable`이 멈추거나 타임아웃 | 서버 중단, 9090 포트 차단, 잘못된 호스트 | [`toki settings sync enable`이 타임아웃됨](#toki-settings-sync-enable이-타임아웃됨) |
| TLS 핸드셰이크 실패, 연결 시 "certificate error" | 자체 서명에 `--insecure` 누락, DNS 미전파 | ["connection refused" 또는 "certificate error"](#connection-refused-또는-certificate-error) |
| 대시보드가 열리지만 사용량 데이터 없음 | 연결된 디바이스 없음, 클라이언트 데몬 미실행, 동기화 멈춤 | [대시보드에 데이터가 표시되지 않음](#대시보드에-데이터가-표시되지-않음) |
| 웹/CLI 로그인 거부 | 관리자 비밀번호 오류, `.env` 변경이 반영 안 됨 | [로그인 시 "invalid credentials"](#로그인-시-invalid-credentials) |
| 로그에 이벤트 스토어 에러 | Fjall 디렉토리 손상, ClickHouse 다운, 디스크 가득 참 | [이벤트 스토어 문제](#이벤트-스토어-문제) |
| 동기화가 반복적으로 끊김 | 네트워크 불안정, 서버 재시작 루프, 인증 만료 | [동기화 재연결 문제](#동기화-재연결-문제) |

배포 시나리오별 이슈는 각 배포 가이드를 참고하세요. [Caddy + DuckDNS](deploy-caddy-duckdns.ko.md) (시나리오 A), [기존 리버스 프록시](deploy-reverse-proxy.ko.md) (시나리오 B), [자체 서명 TLS](deploy-self-signed.ko.md) (시나리오 C), [로컬 / LAN](deploy-local.ko.md) (시나리오 D).

---

## Caddy가 TLS 인증서를 받지 못함

시나리오 A (Caddy + DuckDNS)에 해당합니다.

- DuckDNS 서브도메인이 올바른 IP를 가리키는지 확인: [https://www.duckdns.org](https://www.duckdns.org)에서 확인합니다.
- `.env`의 `DUCKDNS_TOKEN`이 올바른지 확인합니다.
- Caddy 로그 확인: `docker logs toki-caddy`.
- 방화벽에서 포트 443과 9090이 차단되지 않았는지 확인합니다.
- Let's Encrypt는 도메인당 주당 5건의 중복 인증서 발급 한도를 적용합니다. 초과한 경우 잠시 기다린 후 재시도하세요.

---

## `toki settings sync enable`이 타임아웃됨

- 서버가 실행 중인지 확인: `docker compose ps`.
- 포트 9090에 접근 가능한지 확인: `nc -zv yourserver.duckdns.org 9090`.
- 서버의 방화벽 규칙 확인.
- toki-sync-server 로그 확인: `docker logs toki-sync-server`.

---

## "connection refused" 또는 "certificate error"

- 자체 서명 TLS (시나리오 C)에서는 `--insecure` 플래그를 사용합니다.

  ```bash
  toki settings sync enable --server <ip> --insecure
  ```

- 도메인 기반 TLS (시나리오 A)에서는 DNS 전파를 확인합니다: `dig myserver.duckdns.org`.
- `.env`의 `TOKI_EXTERNAL_URL`이 실제 도메인 또는 IP와 일치하는지 확인합니다.

---

## 이벤트 스토어 문제

### Fjall (기본)

- Fjall은 내장되어 있어 별도 컨테이너가 없습니다. 서버 로그를 확인하세요: `docker logs toki-sync-server`.
- `toki-data` 볼륨에 디스크 공간이 충분한지 확인합니다.
- 데이터가 손상된 것으로 보이면 서버를 중지하고 `/data/events.fjall`을 삭제한 후 재시작합니다. 클라이언트가 전체 재동기화를 수행합니다.

### ClickHouse (선택)

- 로그 확인: `docker logs toki-clickhouse`.
- 헬스 체크: `docker exec toki-clickhouse wget -qO- http://localhost:8123/ping`.
- `clickhouse-data` 볼륨에 디스크 공간이 충분한지 확인합니다.

---

## 대시보드에 데이터가 표시되지 않음

- 연결된 디바이스가 있는지 확인: `toki settings sync devices`.
- 클라이언트에서 toki 데몬이 실행 중인지 확인: `toki daemon status`.
- 클라이언트에서 동기화 상태 확인: `toki settings sync status`.
- 서버 로그에서 에러 확인: `docker logs toki-sync-server`.

---

## 로그인 시 "invalid credentials"

- 비밀번호가 `.env`의 `TOKI_ADMIN_PASSWORD`와 일치하는지 확인합니다.
- 관리자 계정은 첫 서버 시작 시 생성됩니다. 그 이후에 `.env`에서 `TOKI_ADMIN_PASSWORD`를 변경해도 저장된 해시는 갱신되지 않습니다. API나 대시보드로 비밀번호를 변경하세요.

---

## 동기화 재연결 문제

- toki 데몬은 연결이 끊기면 지수 백오프 (2초~300초)를 사용합니다.
- 클라이언트 측 동기화 상태 확인: `toki settings sync status`.
- toki 데몬 재시작: `toki daemon restart`.
- 서버 로그에서 인증 에러 확인: `docker logs toki-sync-server`.
