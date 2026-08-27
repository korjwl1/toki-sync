<p align="center">
  <img src="assets/logo.png" alt="toki-sync 로고" width="160" />
</p>

<h1 align="center">toki-sync</h1>

<p align="center">
  <b>Claude Code와 Codex CLI 사용량을 여러 디바이스에서 집계하는 셀프호스트 서버</b><br>
  <a href="https://github.com/korjwl1/toki">toki</a>에서 사용량 이벤트를 받아 중앙에 저장하고, 인증된 쿼리 및 관리 API를 제공합니다.
</p>

<p align="center">
  <a href="https://hub.docker.com/r/korjwl11/toki-sync"><img src="https://img.shields.io/docker/v/korjwl11/toki-sync?sort=semver&label=Docker%20Hub" alt="Docker Hub" /></a>
  <a href="https://hub.docker.com/r/korjwl11/toki-sync"><img src="https://img.shields.io/docker/pulls/korjwl11/toki-sync" alt="Docker Pulls" /></a>
  <a href="https://hub.docker.com/r/korjwl11/toki-sync"><img src="https://img.shields.io/docker/image-size/korjwl11/toki-sync?sort=semver" alt="Docker Image Size" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue" alt="MIT License" /></a>
</p>

<p align="center">
  <a href="README.md">English</a>
</p>

---

## 빠른 시작

`git clone` 불필요. `docker-compose.yml`과 `.env`만 만들면 됩니다.

**1. `docker-compose.yml` 생성**

```yaml
services:
  toki-sync-server:
    image: korjwl11/toki-sync:latest
    container_name: toki-sync-server
    restart: unless-stopped
    ports:
      - "9090:9090"   # 동기화 프로토콜 (TCP)
      - "9091:9091"   # 관리 콘솔 + API (HTTP)
    environment:
      TOKI_ADMIN_PASSWORD: ${TOKI_ADMIN_PASSWORD}
      JWT_SECRET: ${JWT_SECRET}
    volumes:
      - toki-data:/data

volumes:
  toki-data:
```

**2. `.env` 생성**

```bash
TOKI_ADMIN_PASSWORD=강력한-비밀번호로-변경하세요
JWT_SECRET=openssl-rand-base64-32-실행-결과로-변경하세요
```

**3. 시작 및 연결**

```bash
docker compose up -d

# 직접 노출된 포트는 평문 HTTP/TCP입니다. 신뢰하는 로컬 네트워크에서만 사용하세요.
toki settings sync enable --server <서버-IP> --no-tls
```

완료. 이제 모든 기기의 토큰 사용량이 자동으로 동기화됩니다. 이 직접 포트 구성은
인터넷에 공개하지 마세요. 운영 환경에서는 외부 리버스 프록시가 HTTP와 TCP TLS를
종단해야 합니다.

> **공개 TLS가 필요한가요?** [기존 리버스 프록시](docs/deploy-reverse-proxy.ko.md)를
> 사용하세요. 번들 [DuckDNS/Caddy 시나리오](docs/deploy-caddy-duckdns.ko.md)는 현재
> 내부 CA를 사용하며 자동 공개 TLS가 아닙니다.

---

## Docker 이미지

| 항목 | 값 |
|---|---|
| 이미지 | [`korjwl11/toki-sync`](https://hub.docker.com/r/korjwl11/toki-sync) |
| 소스 패키지 버전 | `2.1.0` |
| 플랫폼 | `linux/amd64`, `linux/arm64` |

예시는 `latest`를 사용합니다. 재현 가능한 배포에서는 레지스트리에 실제 존재하는
것을 확인한 Docker 태그를 고정하세요. Cargo 패키지 버전과 같은 Docker 이미지가
반드시 배포됐다는 뜻은 아닙니다.

### 단독 실행 (기본)

**Fjall** (내장 이벤트 스토어) + **SQLite** (메타데이터) 사용. 외부 의존성 없이 단일 컨테이너만으로 동작합니다.

### ClickHouse 연동 (선택)

ClickHouse 컨테이너를 시작하는 것만으로 백엔드가 전환되지는 않습니다.
`config/toki-sync.toml`을 먼저 다음과 같이 바꾼 뒤 Compose 프로필을 켜세요.

```toml
[events]
backend = "clickhouse"
clickhouse_url = "http://clickhouse:8123"
```

```bash
docker compose --profile clickhouse up -d
```

toki-sync가 새 이벤트 읽기와 쓰기에 ClickHouse를 사용하게 됩니다. Fjall과
ClickHouse 사이의 기존 데이터는 자동 이전되지 않습니다. 기존 ClickHouse 설치는
[`docs/CONFIGURATION.ko.md`](docs/CONFIGURATION.ko.md#clickhouse-업그레이드-주의사항)의
업그레이드 주의사항도 확인하세요.

### 현재 브랜치를 소스에서 빌드하기

현재 브랜치는 아직 공개 태그보다 새로운 protocol 타입을 사용합니다. 그래서
`Cargo.toml`이 형제 디렉터리 `../toki_sync_protocol`을 로컬 patch로 참조합니다.
일반적인 `docker build .`은 그 경로를 볼 수 없어 현재 실패합니다. 개발 빌드는 두
저장소를 형제 경로에 둬야 합니다. 릴리즈/Docker 빌드는
`toki-sync-protocol` v1.1.0 태그 생성, 의존성 재고정, 로컬 patch 제거가 먼저입니다.
이미 배포된 이미지는 이 소스 빌드 제약의 영향을 받지 않습니다.

---

## 누구를 위한 건가요?

- **여러 기기 사용?** CLI 또는 [Toki Monitor](https://github.com/korjwl1/toki-monitor)에서 동기화된 사용량을 조회합니다.
- **팀 사용?** 역할 기반 접근 제어로 팀원 간 사용량을 집계합니다.
- **셀프 호스팅?** 데이터가 내 서버에만 남습니다. 텔레메트리 없음, 클라우드 없음.

---

## 작동 방식

```text
[디바이스 A]  [디바이스 B]  [디바이스 C]
toki daemon   toki daemon   toki daemon
     +-- TCP+TLS (bincode) --+
                              v
                      toki-sync 서버
                      |-- TCP :9090 (동기화 프로토콜)
                      |-- HTTP :9091 (인증 + 쿼리 API + 관리 콘솔)
                      +-- SQLite (메타데이터)
                      +-- Fjall (이벤트) 또는 ClickHouse (선택)
```

- **toki 데몬**이 지속 연결을 유지하고, 이벤트를 배치(1,000/배치)로 zstd 압축 후 ACK 기반 흐름 제어로 전송합니다. 배포가 TLS 종단을 제공할 때 TLS가 활성화됩니다
- **toki-sync 서버**가 사용자를 인증하고, SQLite에 메타데이터를 저장하며, 이벤트 스토어에 기록합니다
- **멱등 upsert**는 현재 스키마의 `(device_id, provider, msg_id)`를 사용해 재전송이 중복 집계되지 않게 합니다

---

## 기능

- **멀티 디바이스 동기화** -- TCP 바이너리 프로토콜, zstd 압축, ACK 흐름 제어, 재연결 시 증분 동기화 (delta sync)
- **디바이스 코드 인증** -- 브라우저 기반 device code flow, OIDC (Google, GitHub 등), 비밀번호 로그인
- **관리 콘솔** -- 사용자, 디바이스, 팀, 가입, OIDC, 쿼리 범위 관리
- **팀** -- 역할 기반 접근 제어로 팀 멤버 간 집계 쿼리
- **듀얼 스토리지** -- SQLite (설정 불필요) 또는 PostgreSQL; Fjall (내장) 또는 ClickHouse (대규모)
- **인증된 쿼리 API** -- usage, cost, events, windows를 위한 toki 가상 쿼리 부분집합
- **보안** -- 무차별 대입 방지와 리프레시 토큰 로테이션. TLS는 Caddy 또는 외부 리버스 프록시가 담당

---

## 개인정보 보호 및 보안

- **프롬프트 접근 없음** -- 토큰 수와 메타데이터(모델, 세션 ID, 프로젝트명)만 전송. 프롬프트나 응답은 절대 전송하지 않습니다.
- **배포 방식에 따른 TLS** -- 서버 자체는 내부에서 평문 TCP/HTTP로 수신합니다. 외부 공개 시 번들된 자체 서명 Caddy 프로필 또는 올바르게 설정한 외부 리버스 프록시를 사용하세요.
- **사용자별 데이터 격리** -- 각 사용자는 자신의 데이터만 조회할 수 있습니다.
- **셀프 호스팅** -- 텔레메트리 없음, 클라우드 의존성 없음.

---

## 문서

[배포 가이드](docs/deployment.ko.md)에서 시나리오를 선택한 뒤, 필요할 때 아래 문서를 참고하세요.

| 문서 | 언제 읽나 |
|---|---|
| [배포 가이드](docs/deployment.ko.md) | 인프라에 맞는 시나리오 (A/B/C/D) 선택 |
| [아키텍처와 설계](docs/DESIGN.ko.md) | 구현된 프로토콜, 커서, 스토리지, 제한, 검증 상태 |
| [설정 레퍼런스](docs/CONFIGURATION.ko.md) | 전체 TOML 옵션, 기본값, 환경 변수 |
| [HTTP API 레퍼런스](docs/API.ko.md) | 전체 엔드포인트, 요청/응답 예시, 인증 |
| [커스텀 대시보드](docs/custom-dashboard.ko.md) | toki-sync 쿼리 API 위에 자체 UI 구축 |
| [백업과 복원](docs/backup.ko.md) | 볼륨 구조, 핫/콜드 백업, 복원 |
| [문제 해결](docs/troubleshooting.ko.md) | 연결, TLS, 쿼리, 스토리지, 동기화 문제 진단 |
| [기여 가이드](CONTRIBUTING.ko.md) | 개발 환경, 브랜치 이름, 커밋 규칙, DCO |

---

## 연결 해제

```bash
toki settings sync disable              # 원격 데이터 삭제 여부를 묻습니다
toki settings sync disable --delete     # 서버에서 이 디바이스의 데이터를 삭제합니다
toki settings sync disable --keep       # 원격 데이터를 유지하고 로컬에서만 비활성화합니다
```

---

## Sponsor

<a href="https://github.com/sponsors/korjwl1">
  <img src="https://img.shields.io/badge/Sponsor-%E2%9D%A4-pink?style=for-the-badge&logo=github" alt="Sponsor" />
</a>

toki-sync가 유용하다면 스폰서로 개발을 지원해 주세요.

MIT 라이선스는 상업적 사용을 허용합니다. 스폰서는 선택 사항이며 지속적인 유지보수를
지원합니다.

---

## 라이선스

[MIT](LICENSE)
