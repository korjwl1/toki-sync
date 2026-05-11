# 배포 가이드

기존 인프라에 맞는 시나리오 하나를 골라 연결된 가이드를 따라가세요. 모든 시나리오는 동일한 `docker-compose.yml`을 사용하며, 활성화하는 프로파일과 TLS 처리 방식만 다릅니다.

## 의사결정 흐름

- nginx, Traefik 등 리버스 프록시를 이미 운영 중인가요? → [시나리오 B: 기존 리버스 프록시](deploy-reverse-proxy.ko.md).
- 공개 IP는 있지만 도메인이 없나요? → [시나리오 C: 자체 서명 TLS](deploy-self-signed.ko.md).
- 무료 도메인과 자동 Let's Encrypt가 필요한가요? → [시나리오 A: Caddy + DuckDNS](deploy-caddy-duckdns.ko.md).
- 개발용 localhost 환경인가요? → [시나리오 D: 로컬 / LAN](deploy-local.ko.md).

## 비교

| 시나리오 | TLS | 도메인 | 리버스 프록시 | 적합한 환경 |
|---|---|---|---|---|
| [A — Caddy + DuckDNS](deploy-caddy-duckdns.ko.md) | 자동 (Let's Encrypt) | DuckDNS 무료 서브도메인 | 내장 Caddy | 새 공개 서버, 기존 프록시 없음 |
| [B — 기존 리버스 프록시](deploy-reverse-proxy.ko.md) | 기존 프록시 담당 | 자체 도메인 | nginx, Traefik 등 | 이미 TLS를 종단하는 서버 |
| [C — 자체 서명 TLS](deploy-self-signed.ko.md) | 자체 서명 (Caddy `tls internal`) | 없음 (IP 전용) | 내장 Caddy | 홈 랩, LAN 전용 |
| [D — 로컬 / LAN](deploy-local.ko.md) | 없음 | 없음 | 없음 | 개발과 테스트 전용 |

## 운영

배포 후에는 다음 문서를 참고하세요.

- [백업과 복원](backup.ko.md) — 볼륨 구조, `tar` 콜드 백업, ClickHouse 백업, 복원.
- [문제 해결](troubleshooting.ko.md) — TLS, 동기화, 대시보드, 인증 문제의 증상별 매핑.
- [설정 레퍼런스](CONFIGURATION.ko.md) — 모든 TOML 키, 기본값, 환경 변수.
