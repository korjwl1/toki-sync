# 시나리오 C: 자체 서명 TLS (IP 전용)

도메인이 없는 서버 (예: 로컬 IP의 홈 랩)에 적합합니다. Caddy가 자체 서명 인증서를 자동 생성합니다.

> 다른 시나리오가 더 맞으면 [Caddy + DuckDNS](deploy-caddy-duckdns.ko.md) (무료 도메인 + Let's Encrypt), [기존 리버스 프록시](deploy-reverse-proxy.ko.md) (nginx/Traefik 운영 중), [로컬 / LAN](deploy-local.ko.md) (개발 테스트)을 참고하세요.

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

`.env`를 편집합니다 — `DUCKDNS_TOKEN`과 `TOKI_DOMAIN`을 비워두면 Caddy가 자체 서명 모드로 동작합니다.

```bash
TOKI_ADMIN_PASSWORD=your-strong-password
JWT_SECRET=$(openssl rand -base64 32)
TOKI_EXTERNAL_URL=https://192.168.1.100
# DUCKDNS_TOKEN 미설정
# TOKI_DOMAIN 미설정 — Caddy가 tls internal (자체 서명) 사용
```

번들된 `caddy/Caddyfile`이 이 모드를 그대로 지원합니다. 관련 부분은 다음과 같습니다.

```caddyfile
{$TOKI_DOMAIN:localhost} {
    tls {$TLS_MODE:internal}

    reverse_proxy toki-sync-server:9091
}
```

`TOKI_DOMAIN`이 비어 있으면 사이트 주소가 `localhost`로 떨어지고, `TLS_MODE` 기본값인 `internal`로 Caddy가 자체 서명 인증서를 생성·제공합니다. Caddyfile 수정은 필요 없습니다.

---

## 2단계: 배포

**Caddy 사용** (자체 서명 모드):

```bash
docker compose --profile caddy up -d
```

**Caddy 없이** (포트 직접 노출):

```bash
docker compose up -d
```

Caddy를 쓰지 않으면 `docker-compose.yml`에 포트 매핑을 추가합니다.

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

자체 서명 인증서를 수락하려면 클라이언트가 `--insecure` 플래그를 사용해야 합니다.

```bash
toki settings sync enable --server 192.168.1.100 --insecure
toki settings sync status
```
