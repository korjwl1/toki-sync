# 시나리오 D: 로컬 / LAN (TLS 없음)

개발이나 테스트용 localhost 배포입니다. 프로덕션에서는 권장하지 않습니다.

> 다른 환경이면 공개 TLS용 [기존 리버스 프록시](deploy-reverse-proxy.ko.md) 또는
> 신뢰하는 LAN용 [내부 CA TLS](deploy-self-signed.ko.md)를 참고하세요.

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
# 현재 source build는 Docker context에서 형제 protocol patch를 해석할 수 없습니다.
# 공개 이미지를 사용하고 build를 명시적으로 금지합니다.
docker compose pull toki-sync-server
docker compose up -d --no-build
```

---

## 4단계: 연결

```bash
toki settings sync enable --server localhost --no-tls
toki settings sync status
```
