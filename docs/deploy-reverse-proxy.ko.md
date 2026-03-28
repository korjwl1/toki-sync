# 시나리오 B: 기존 리버스 프록시

이미 nginx, Traefik 등으로 TLS를 처리하고 있는 서버에 적합합니다.

---

## 사전 요구사항

- 서버에 **Docker** 및 **Docker Compose v2** 설치
- 유효한 TLS 인증서가 있는 기존 리버스 프록시
- 서버를 가리키는 도메인 이름

---

## 1단계: 클론 및 설정

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
TOKI_EXTERNAL_URL=https://yourserver.example.com
# DUCKDNS_TOKEN 불필요 — 기존 프록시가 TLS를 처리합니다
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

toki-sync-server와 VictoriaMetrics만 시작됩니다 (Caddy 없음).

---

## 4단계: 프록시 설정

두 가지 유형의 트래픽을 toki-sync로 포워딩하세요:

| 트래픽 | 업스트림 |
|--------|----------|
| HTTPS :443 (HTTP) | `http://127.0.0.1:9091` |
| TLS :9090 (TCP 스트림) | `127.0.0.1:9090` |

### nginx

```nginx
# HTTP API + 대시보드
server {
    listen 443 ssl;
    server_name yourserver.example.com;

    ssl_certificate     /path/to/cert.pem;
    ssl_certificate_key /path/to/key.pem;

    location / {
        proxy_pass http://127.0.0.1:9091;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
    }
}

# TCP 동기화 프로토콜 (nginx stream 모듈 필요)
stream {
    server {
        listen 9090 ssl;
        ssl_certificate     /path/to/cert.pem;
        ssl_certificate_key /path/to/key.pem;
        proxy_pass 127.0.0.1:9090;
    }
}
```

### Traefik

```yaml
# traefik 동적 설정
http:
  routers:
    toki-sync-http:
      rule: "Host(`yourserver.example.com`)"
      service: toki-sync-http
      tls:
        certResolver: letsencrypt
  services:
    toki-sync-http:
      loadBalancer:
        servers:
          - url: "http://127.0.0.1:9091"

tcp:
  routers:
    toki-sync-tcp:
      rule: "HostSNI(`yourserver.example.com`)"
      service: toki-sync-tcp
      tls:
        certResolver: letsencrypt
  services:
    toki-sync-tcp:
      loadBalancer:
        servers:
          - address: "127.0.0.1:9090"
```

---

## 5단계: 디바이스 연결

[toki](https://github.com/korjwl1/toki)가 설치된 아무 기기에서:

```bash
toki settings sync enable --server yourserver.example.com:9090 --username admin
toki settings sync status
```
