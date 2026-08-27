# 시나리오 A: DuckDNS와 공개 TLS

이 시나리오는 **현재 저장소 revision에서 turnkey가 아닙니다**. DuckDNS는 hostname을
제공할 수 있지만 번들 Caddyfile은 Caddy 내부 CA를 명시적으로 사용합니다.

```caddyfile
{$TOKI_DOMAIN:localhost} {
    tls {$TLS_MODE:internal}
    reverse_proxy toki-sync-server:9091
}
```

`docker-compose.yml`은 `TLS_MODE`를 설정하지 않으므로 기본값 `internal`이 항상
선택됩니다. `DUCKDNS_TOKEN`은 컨테이너에 전달되지만 Caddy 이미지와 Caddyfile 모두
DuckDNS DNS provider를 포함하거나 사용하지 않습니다. 이전 가이드를 따라도 Let's
Encrypt가 아니라 신뢰되지 않는 인증서가 생성됩니다.

## 안전한 선택

- 권장: 이미 신뢰할 수 있는 인증서를 발급하며 HTTPS :443과 TLS TCP :9090을 모두
  proxy할 수 있는 [외부 리버스 프록시](deploy-reverse-proxy.ko.md)를 사용하세요.
- 신뢰하는 LAN/home lab에서는 toki `--insecure`와 함께 문서화된
  [내부 CA 모드](deploy-self-signed.ko.md)를 사용하세요.
- custom Caddy build/config를 관리한다면 강제 내부 CA를 제거하고 지원되는 ACME
  challenge와 두 listener용 인증서를 구성한 뒤 독립적으로 검증하세요. 그 customization은
  이 저장소의 테스트 범위 밖입니다.

## DuckDNS hostname만 사용하기

DuckDNS 이름을 서버로 향하게 하는 것은 가능합니다. 동적 주소라면 별도로 갱신하세요.

```bash
*/5 * * * * curl -s "https://www.duckdns.org/update?domains=myserver&token=YOUR_TOKEN&ip=" > /dev/null
```

이 명령은 DNS만 갱신하며 toki-sync를 설정하거나 인증서를 발급하지 않습니다.

## 공개 이미지와 소스 빌드

현재 toki-sync 브랜치는 형제 protocol patch가 활성화되어 번들 Dockerfile로 빌드할
수 없습니다. 저장소 Compose 파일을 사용한다면 공개 server 이미지를 pull하고 source
build를 금지하세요.

```bash
docker compose pull toki-sync-server
docker compose up -d --no-build
```

custom TLS proxy가 번들 `caddy` 서비스라면 먼저 그 서비스만
`docker compose build caddy`로 빌드한 뒤 `up --no-build`를 실행합니다. 실제 인증서
chain을 검증하지 않았다면 DuckDNS/Let's Encrypt 구성이라고 보고하지 마세요.

관리 콘솔 경로는 `/admin`이며 `/dashboard` 경로는 없습니다. 원격 사용량 쿼리는
다음을 사용합니다.

```bash
toki query --remote 'sum by (model)(toki_tokens_total)'
```
