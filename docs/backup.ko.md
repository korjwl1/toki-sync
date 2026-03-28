# 백업 및 복원

## 데이터 볼륨

| 볼륨 | 경로 | 내용 | 손실 시 |
|------|------|------|---------|
| `toki-data` | `/data` | SQLite (사용자, 디바이스, 커서) | 재로그인 + 전체 재동기화 필요 |
| `vm-data` | `/vm-data` | VictoriaMetrics 시계열 데이터 | **복구 불가** — 모든 과거 사용량 데이터 손실 |
| `caddy-data` | `/data` | Let's Encrypt 인증서 | 자동 재발급 (주당 5회 제한) |

`vm-data`가 핵심 볼륨입니다. 손실되면 모든 과거 토큰 사용량 데이터가 영구적으로 사라집니다.

---

## Bind Mount (백업에 권장)

백업 접근을 쉽게 하려면 `docker-compose.yml`에서 named volume 대신 bind mount를 사용하세요:

```yaml
volumes:
  - ./data/vm:/vm-data
  - ./data/toki:/data
```

---

## VictoriaMetrics 핫 스냅샷

VictoriaMetrics는 다운타임 없이 스냅샷을 생성할 수 있습니다:

```bash
# 스냅샷 생성
docker exec victoriametrics wget -qO- http://localhost:8428/snapshot/create

# 스냅샷 목록
docker exec victoriametrics wget -qO- http://localhost:8428/snapshot/list

# 스냅샷 삭제
docker exec victoriametrics wget -qO- "http://localhost:8428/snapshot/delete?snapshot=SNAPSHOT_NAME"
```

스냅샷은 `vm-data` 볼륨의 `snapshots/` 디렉토리에 저장됩니다.

자세한 내용은 [VictoriaMetrics 백업 문서](https://docs.victoriametrics.com/single-server-victoriametrics/#backups)를 참고하세요.

---

## VM / VPS 디스크 스냅샷

소규모 배포에 가장 간단한 방식입니다:

1. 컨테이너 중지: `docker compose down`
2. 클라우드 제공자 콘솔에서 전체 VM/VPS 디스크 스냅샷
3. 재시작: `docker compose --profile caddy up -d`

데이터베이스, 시계열, 인증서 모두를 캡처합니다.

---

## 수동 파일 백업

bind mount를 사용하는 경우:

```bash
# 일관성을 위해 컨테이너 중지
docker compose down

# 데이터 아카이브
tar czf toki-sync-backup-$(date +%Y%m%d).tar.gz ./data/

# 재시작
docker compose --profile caddy up -d
```

---

## 복원

1. 컨테이너 중지: `docker compose down`
2. 데이터 디렉토리를 백업으로 교체
3. 재시작: `docker compose --profile caddy up -d`
4. 클라이언트가 자동으로 재연결됩니다 (toki 데몬이 지수 백오프로 재시도)
