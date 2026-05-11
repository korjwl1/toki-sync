# 시나리오 D: 로컬 / LAN (TLS 없음)

개발이나 테스트용 localhost 배포입니다. 프로덕션에서는 권장하지 않습니다.

> 다른 환경이면 [Caddy + DuckDNS](deploy-caddy-duckdns.ko.md) (프로덕션 자동 TLS), [기존 리버스 프록시](deploy-reverse-proxy.ko.md) (이미 운영 중), [자체 서명 TLS](deploy-self-signed.ko.md) (IP 전용 서버)을 참고하세요.

---

## 사전 요구사항

- **Docker** 및 **Docker Compose v2**

---

## 1단계: 클론 및 설정

```bash
git clone https://github.com/korjwl1/toki-sync.git
cd toki-sync

cp .env.example .env
cp config/toki-sync.toml.example config/toki-sync.toml
```

로컬 사용을 위한 `.env` 편집:

```bash
TOKI_ADMIN_PASSWORD=dev-password
JWT_SECRET=dev-secret-change-in-production
TOKI_EXTERNAL_URL=http://localhost:9091
```

---

## 2단계: 포트 노출

`docker-compose.yml`의 `toki-sync-server`에 포트 매핑을 추가합니다:

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

## 3단계: 배포

```bash
docker compose up -d
```

---

## 4단계: 연결

```bash
toki settings sync enable --server localhost --no-tls
toki settings sync status
```
