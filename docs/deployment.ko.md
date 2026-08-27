# 배포 가이드

인프라에 맞는 시나리오를 고르세요. 현재 source checkout에는 중요한 제약 두 가지가
있습니다.

1. protocol 1.1.0 형제 patch가 활성화된 동안 `docker build .`은 실패합니다. 공개
   toki-sync 이미지를 `docker compose pull`과 `--no-build`로 사용하거나 protocol
   tag/re-pin 릴리즈 작업을 기다리세요.
2. 번들 Caddyfile은 Caddy 내부 CA를 강제합니다. `DUCKDNS_TOKEN`은 사용되지 않으므로
   현재 저장소만으로 DuckDNS/Let's Encrypt 공개 인증서를 자동 발급할 수 없습니다.

## 의사결정 흐름

- nginx, Traefik 등 리버스 프록시를 이미 운영 중인가요? → [시나리오 B: 기존 리버스 프록시](deploy-reverse-proxy.ko.md).
- 공개 IP는 있지만 도메인이 없나요? → [시나리오 C: 자체 서명 TLS](deploy-self-signed.ko.md).
- DuckDNS와 공개 Let's Encrypt가 필요한가요? → [시나리오 A 상태와 필요한 외부 TLS 작업](deploy-caddy-duckdns.ko.md). 이번 revision에서는 turnkey가 아닙니다.
- 개발용 localhost 환경인가요? → [시나리오 D: 로컬 / LAN](deploy-local.ko.md).

## 비교

| 시나리오 | TLS | 도메인 | 리버스 프록시 | 적합한 환경 |
|---|---|---|---|---|
| [A — Caddy + DuckDNS](deploy-caddy-duckdns.ko.md) | 번들 Caddy에 미연결 | DuckDNS 무료 서브도메인 | 수정/custom proxy 필요 | 이번 revision에서 turnkey 아님 |
| [B — 기존 리버스 프록시](deploy-reverse-proxy.ko.md) | 기존 프록시 담당 | 자체 도메인 | nginx, Traefik 등 | 이미 TLS를 종단하는 서버 |
| [C — 자체 서명 TLS](deploy-self-signed.ko.md) | 내부 CA 인증서(Caddy `tls internal`) | IP/host | 내장 Caddy | 홈 랩, LAN 전용 |
| [D — 로컬 / LAN](deploy-local.ko.md) | 없음 | 없음 | 없음 | 개발과 테스트 전용 |

서버 프로세스는 bound port에서 항상 평문 TCP/HTTP를 사용합니다. 시나리오 D를 인터넷에
직접 공개하지 마세요.

## 운영

배포 후에는 다음 문서를 참고하세요.

- [백업과 복원](backup.ko.md) — 볼륨 구조, `tar` 콜드 백업, ClickHouse 백업, 복원.
- [문제 해결](troubleshooting.ko.md) — TLS, 동기화, 대시보드, 인증 문제의 증상별 매핑.
- [설정 레퍼런스](CONFIGURATION.ko.md) — 모든 TOML 키, 기본값, 환경 변수.
