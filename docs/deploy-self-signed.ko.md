# 시나리오 C: 자체 서명 TLS (IP 전용)

도메인 이름이 없는 서버(예: 로컬 IP의 홈 랩)에 적합합니다. Caddy가 자체 서명 인증서를 자동 생성합니다.

---

## 사전 요구사항

- 서버에 **Docker** 및 **Docker Compose v2** 설치
- 알려진 IP 주소 (공개 또는 LAN)를 가진 서버

---

## 1단계: 클론 및 설정

```bash
git clone https://github.com/korjwl1/toki-sync.git
cd toki-sync

cp .env.example .env
cp config/toki-sync.toml.example config/toki-sync.toml
```

`.env` 편집 — `DUCKDNS_TOKEN`을 비워둡니다:

```bash
TOKI_ADMIN_PASSWORD=your-strong-password
JWT_SECRET=$(openssl rand -base64 32)
TOKI_EXTERNAL_URL=https://192.168.1.100
# DUCKDNS_TOKEN 미설정 — Caddy가 tls internal (자체 서명) 사용
```

---

## 2단계: 배포

**Caddy 사용** (자체 서명 모드):

```bash
docker compose --profile caddy up -d
```

> 참고: Caddyfile이 DuckDNS 토큰이 없을 때 `tls internal`을 사용하도록 구성되어 있어야 합니다. 템플릿 로직은 `caddy/Caddyfile`을 참고하세요.

**Caddy 없이** (포트 직접 노출):

```bash
docker compose up -d
```

`docker-compose.yml`에 포트 매핑을 추가합니다:

```yaml
services:
  toki-sync-server:
    ports:
      - "9091:9091"
      - "9090:9090"
    networks:
      - internal
      - external
```

---

## 3단계: 디바이스 연결

클라이언트는 자체 서명 인증서를 수락하기 위해 `--insecure` 플래그를 사용해야 합니다:

```bash
toki settings sync enable --server 192.168.1.100:9090 --insecure --username admin
toki settings sync status
```
