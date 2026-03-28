# 시나리오 A: Caddy + DuckDNS

기존 리버스 프록시가 없는 서버에 적합합니다. Caddy가 Let's Encrypt를 통해 TLS 인증서를 자동으로 관리하고, DuckDNS가 서버를 가리키는 무료 도메인 이름을 제공합니다.

---

## 사전 요구사항

- 서버에 **Docker** 및 **Docker Compose v2** 설치
- 공개 IP 주소를 가진 서버
- 포트 **443**과 **9090** 사용 가능 (방화벽에서 차단되지 않음)

---

## 1단계: DuckDNS 도메인 만들기

1. [https://www.duckdns.org](https://www.duckdns.org)에 접속합니다
2. Google, GitHub, Twitter, 또는 Reddit으로 로그인합니다
3. **서브도메인 생성** — 이름을 입력하고 (예: `myserver`) "add domain"을 클릭합니다. `myserver.duckdns.org`가 생성됩니다
4. **토큰 복사** — 로그인 후 페이지 상단에 표시됩니다. `a1b2c3d4-e5f6-7890-abcd-ef1234567890` 형식입니다
5. **서버 IP 연결** — DuckDNS 페이지에서 서브도메인 옆에 서버의 공개 IP를 입력하고 "update ip"를 클릭합니다

### 동적 IP인 경우

서버 IP가 변경되는 경우 (예: 가정용 인터넷), DuckDNS가 항상 현재 IP를 가리키도록 자동 업데이트를 설정하세요.

**방법 1: cron job** (가장 간단)

```bash
# crontab에 추가 (crontab -e) — 5분마다 업데이트
*/5 * * * * curl -s "https://www.duckdns.org/update?domains=myserver&token=YOUR_TOKEN&ip=" > /dev/null
```

**방법 2: Docker 컨테이너** (toki-sync와 함께 실행)

```bash
docker run -d --name duckdns-updater --restart unless-stopped \
  -e SUBDOMAINS=myserver \
  -e TOKEN=YOUR_TOKEN \
  lscr.io/linuxserver/duckdns:latest
```

---

## 2단계: 클론 및 설정

```bash
git clone https://github.com/korjwl1/toki-sync.git
cd toki-sync

cp .env.example .env
cp config/toki-sync.toml.example config/toki-sync.toml
```

`.env`를 편집합니다:

```bash
# 관리자 비밀번호 — toki 클라이언트에서 로그인할 때 사용합니다
TOKI_ADMIN_PASSWORD=your-strong-password

# JWT 서명 시크릿 — 랜덤으로 생성하세요:
JWT_SECRET=$(openssl rand -base64 32)

# DuckDNS 도메인 (1단계에서 만든 것과 일치해야 합니다)
TOKI_EXTERNAL_URL=https://myserver.duckdns.org

# DuckDNS 토큰 (1단계에서 복사한 것)
DUCKDNS_TOKEN=a1b2c3d4-e5f6-7890-abcd-ef1234567890
```

---

## 3단계: 배포

```bash
docker compose --profile caddy up -d
```

3개의 컨테이너가 시작됩니다:

| 컨테이너 | 용도 | 포트 |
|----------|------|------|
| **toki-sync-server** | 동기화 프로토콜 + 인증 API | TCP :9090, HTTP :9091 (내부) |
| **VictoriaMetrics** | 시계열 저장소 | 내부 전용 |
| **Caddy** | TLS 종단 | :443 (HTTPS), :9090 (TLS TCP) |

---

## 4단계: 확인

```bash
# 모든 컨테이너가 실행 중인지 확인
docker compose --profile caddy ps

# 서버 헬스 엔드포인트 확인
curl https://myserver.duckdns.org/health
# 반환값: {"status":"ok"}
```

브라우저에서 대시보드를 엽니다: `https://myserver.duckdns.org/dashboard`

---

## 5단계: 디바이스 연결

[toki](https://github.com/korjwl1/toki)가 설치된 아무 기기에서:

```bash
toki settings sync enable --server myserver.duckdns.org:9090 --username admin
# 프롬프트에서 TOKI_ADMIN_PASSWORD 입력

toki settings sync status
# 표시: connected

toki settings sync devices
# 등록된 모든 디바이스 목록
```

동기화하려는 모든 기기에서 반복합니다.

디바이스 연결을 해제하려면:

```bash
toki settings sync disable              # 원격 데이터 삭제 여부를 묻습니다
toki settings sync disable --delete     # 서버에서 이 디바이스의 데이터를 삭제합니다
toki settings sync disable --keep       # 원격 데이터를 유지하고 로컬에서만 비활성화합니다
```

---

## 이후 동작

- toki 데몬이 토큰 사용 이벤트를 배치로 모아 자동으로 동기화합니다
- 디바이스가 오프라인이 되면 이벤트가 로컬에 누적되고, 재연결 시 delta-sync됩니다
- `https://myserver.duckdns.org/dashboard`에서 집계 데이터를 확인합니다
- CLI에서 쿼리: `toki report query --remote 'sum by (model)(toki_tokens_total)'`
- [Toki Monitor](https://github.com/korjwl1/toki-monitor)에서 보기: 대시보드 툴바에서 로컬/서버 모드를 전환합니다
