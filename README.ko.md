<p align="center">
  <img src="assets/logo.png" alt="toki-sync 로고" width="160" />
</p>

<h1 align="center">toki-sync</h1>

<p align="center">
  <b>멀티 디바이스 토큰 사용량 동기화 서버</b><br>
  모든 기기의 AI 도구 사용량을 수집하고, 내장 이벤트 스토어(Fjall)에 저장, 통합 대시보드를 제공합니다.
</p>

<p align="center">
  <a href="https://github.com/korjwl1/toki">toki</a> 생태계의 일부입니다.
</p>

<p align="center">
  <a href="README.md">English</a>
</p>

---

## Quick Start

```bash
git clone https://github.com/korjwl1/toki-sync.git
cd toki-sync
cp .env.example .env
cp config/toki-sync.toml.example config/toki-sync.toml
```

`.env` 편집:

```bash
TOKI_ADMIN_PASSWORD=your-strong-password
JWT_SECRET=$(openssl rand -base64 32)
TOKI_EXTERNAL_URL=https://myserver.duckdns.org
DUCKDNS_TOKEN=your-duckdns-token
```

배포 및 연결:

```bash
docker compose --profile caddy up -d

# toki가 설치된 아무 기기에서 (브라우저를 열어 인증)
toki settings sync enable --server myserver.duckdns.org
```

완료. 토큰 사용량이 자동으로 동기화됩니다.

연결을 해제하려면:

```bash
toki settings sync disable              # 원격 데이터 삭제 여부를 묻습니다
toki settings sync disable --delete     # 서버에서 이 디바이스의 데이터를 삭제합니다
toki settings sync disable --keep       # 원격 데이터를 유지하고 로컬에서만 비활성화합니다
```

> DuckDNS가 처음이신가요? [Caddy + DuckDNS 가이드](docs/deploy-caddy-duckdns.ko.md)에서 가입부터 검증까지 모든 단계를 안내합니다.

---

## 누구를 위한 건가요?

- **여러 기기에서 AI 도구를 사용하나요?** 모든 토큰 사용량을 한 곳에서 확인 — 웹 대시보드나 [Toki Monitor](https://github.com/korjwl1/toki-monitor)에서 시각화할 수 있습니다.

- **팀 사용량 대시보드가 필요하나요?** 팀 범위 쿼리와 역할 기반 접근 제어로 팀원들의 토큰 사용량을 집계합니다.

- **셀프 호스팅 솔루션이 필요하나요?** `docker compose up` 한 번으로 자동 TLS가 포함된 완전한 동기화 서버를 구축합니다. 클라우드 의존성 없음, 텔레메트리 없음.

---

## 작동 방식

```
[디바이스 A]  [디바이스 B]  [디바이스 C]
toki daemon   toki daemon   toki daemon
     +-- TCP+TLS (bincode) --+
                              v
                      toki-sync 서버
                      |-- TCP :9090 (동기화 프로토콜)
                      |-- HTTP :9091 (인증 + 대시보드)
                      +-- SQLite / PostgreSQL (메타데이터)
                      +-- EventStore: Fjall (내장) 또는 ClickHouse (선택)
```

- **toki 데몬**이 persistent TLS 연결을 유지하고, 이벤트를 배치로 모아 (1,000/배치) zstd 압축 후 ACK 기반 흐름 제어로 전송
- **toki-sync 서버**가 사용자를 인증하고, 메타데이터를 SQLite/PostgreSQL에 저장하며, 이벤트를 EventStore(기본: Fjall, 선택: ClickHouse)에 기록
- **EventStore**가 msg_id 기반 중복 제거 처리 (Fjall: idx_msg 고유 인덱스, ClickHouse: ReplacingMergeTree)
- **PromQL 프록시** (선택, VictoriaMetrics 필요) 사용자별 label을 주입하여 데이터 격리

---

## 기능

- **멀티 디바이스 동기화** — TCP 바이너리 프로토콜, zstd 압축, ACK 흐름 제어, 재연결 시 delta-sync
- **Device code 인증** — 브라우저 기반 device code flow, OIDC (Google, GitHub 등), 비밀번호 로그인
- **PromQL 프록시** (선택) — 사용자별 label injection으로 데이터 격리. toki CLI `--remote` 및 Toki Monitor와 호환. 외부 VictoriaMetrics 필요
- **웹 대시보드** — 차트 패널, 시간 범위 선택, 디바이스 목록, 팀 뷰
- **팀 / 조직** — 팀 멤버 간 집계 쿼리
- **듀얼 데이터베이스 백엔드** — SQLite (기본, 설정 불필요) 또는 PostgreSQL (대규모 운영용)
- **Docker 배포** — Caddy 프로필로 자동 TLS, 또는 기존 리버스 프록시 사용
- **무차별 대입 방지** — 시도 횟수 제한, 잠금 윈도우, IP 기반 추적
- **리프레시 토큰 로테이션** — 일회용 로테이션으로 안전한 토큰 갱신

---

## 개인정보 보호 및 보안

- **프롬프트 접근 없음** — 토큰 수와 메타데이터(모델, 세션 ID, 프로젝트명)만 전송됩니다. 프롬프트나 응답은 절대로 전송하지 않습니다.
- **모든 곳에 TLS** — 모든 동기화 트래픽이 암호화됩니다. Caddy가 Let's Encrypt를 통해 인증서를 자동 관리합니다.
- **사용자별 데이터 격리** — 각 사용자는 자신의 데이터만 조회할 수 있습니다. PromQL 프록시(선택)는 VictoriaMetrics 호환을 위해 사용자 label을 주입합니다.
- **셀프 호스팅** — 데이터가 사용자의 서버에만 남습니다. 텔레메트리 없음, 클라우드 의존성 없음.

---

## 배포

| 시나리오 | 가이드 | 설명 |
|----------|--------|------|
| Caddy + DuckDNS | [가이드](docs/deploy-caddy-duckdns.ko.md) | 무료 도메인으로 원클릭 TLS (권장) |
| 기존 프록시 | [가이드](docs/deploy-reverse-proxy.ko.md) | nginx, Traefik 등 |
| 자체 서명 TLS | [가이드](docs/deploy-self-signed.ko.md) | IP 전용 서버, 도메인 없음 |
| 로컬 / LAN | [가이드](docs/deploy-local.ko.md) | 개발 및 테스트 |

참고: [백업 및 복원](docs/backup.ko.md) | [문제 해결](docs/troubleshooting.ko.md)

---

## 문서

| 문서 | 설명 |
|------|------|
| **[아키텍처 & 설계](docs/DESIGN.ko.md)** | Sync 프로토콜, 커서 관리, 보안 모델, 스케일링 가이드 |
| **[설정 레퍼런스](docs/CONFIGURATION.ko.md)** | 전체 TOML 섹션, 필드, 기본값, 환경변수 |
| **[HTTP API 레퍼런스](docs/API.ko.md)** | 전체 엔드포인트, 요청/응답 예시, 인증 |

---

## 기술 스택

| 용도 | 선택 | 근거 |
|------|------|------|
| HTTP 프레임워크 | axum 0.7 | 비동기, tower 미들웨어 생태계 |
| 비동기 런타임 | tokio | 전기능 비동기 I/O |
| 데이터베이스 | sqlx 0.8 (SQLite + PostgreSQL) | 컴파일 타임 쿼리 체크, 듀얼 백엔드 |
| 이벤트 스토어 | Fjall (내장) / ClickHouse (선택) | 무의존성 기본값, 확장 가능한 옵션 |
| 인증 | jsonwebtoken 9 + bcrypt | JWT 액세스/리프레시 토큰, 안전한 비밀번호 해싱 |
| OIDC | reqwest + 수동 discovery | 무거운 프레임워크 없는 표준 OIDC 플로우 |
| Sync 프로토콜 | toki-sync-protocol (공유 crate) | Wire-compatible 타입, bincode 직렬화 |
| 압축 | zstd 0.13 | 동기화 프로토콜용 고속 배치 압축 |
| 직렬화 | bincode (sync), serde_json (API), toml (config) | 성능을 위한 바이너리, 상호운용을 위한 JSON |
| 로깅 | tracing + tracing-subscriber | JSON 출력 옵션을 지원하는 구조화된 로깅 |
| 설정 | toml 0.8 + `${ENV}` 확장 | 간단하고 읽기 쉬운 서버 설정 |

---

## Sponsor

<a href="https://github.com/sponsors/korjwl1">
  <img src="https://img.shields.io/badge/Sponsor-%E2%9D%A4-pink?style=for-the-badge&logo=github" alt="Sponsor" />
</a>

toki-sync가 유용하다면 스폰서로 개발을 지원해 주세요.

유료 제품에서의 상업적 사용은 스폰서 등록 또는 [연락](mailto:korjwl1@gmail.com)을 부탁드립니다.

---

## 라이선스

[FSL-1.1-Apache-2.0](LICENSE)
