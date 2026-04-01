# 백업 및 복원

## 데이터 볼륨

| 볼륨 | 경로 | 내용 | 손실 시 |
|------|------|------|---------|
| `toki-data` | `/data` | SQLite (사용자, 디바이스, 커서) + Fjall 이벤트 스토어 | 재로그인 + 전체 재동기화 필요 |
| `clickhouse-data` | `/var/lib/clickhouse` | ClickHouse 이벤트 데이터 (`--profile clickhouse` 사용 시만) | 클라이언트 재동기화로 복구 가능 |
| `caddy-data` | `/data` | Let's Encrypt 인증서 | 자동 재발급 (주당 5회 제한) |

기본 Fjall 백엔드에서는 `toki-data`에 메타데이터와 이벤트가 모두 포함됩니다. 손실되면 클라이언트가 재연결 시 로컬 이력에서 전체 재동기화를 수행합니다. ClickHouse 사용 시 이벤트 데이터는 `clickhouse-data`에 별도로 저장됩니다.

---

## Bind Mount (백업에 권장)

백업 접근을 쉽게 하려면 `docker-compose.yml`에서 named volume 대신 bind mount를 사용하세요:

```yaml
volumes:
  - ./data/toki:/data
```

---

## Fjall 백업 (기본 백엔드)

Fjall은 데이터를 디렉토리의 파일로 저장합니다. 백업하려면:

```bash
# 일관성을 위해 컨테이너 중지
docker compose down

# 데이터 디렉토리 아카이브
tar czf toki-sync-backup-$(date +%Y%m%d).tar.gz ./data/

# 재시작
docker compose --profile caddy up -d
```

또는 핫 백업(서버 실행 중)으로 Fjall 디렉토리를 복사할 수 있습니다. Fjall은 실행 중 복사해도 안전한 LSM-tree 구조를 사용하지만, 서버를 중지하면 완전한 일관성을 보장합니다.

---

## ClickHouse 백업 (선택적 백엔드)

ClickHouse를 이벤트 스토어로 사용하는 경우:

```bash
# clickhouse-backup 도구 사용
docker exec toki-clickhouse clickhouse-backup create backup_$(date +%Y%m%d)

# 또는 clickhouse-client로 내보내기
docker exec toki-clickhouse clickhouse-client --query "SELECT * FROM events FORMAT Native" > events_backup.bin
```

자세한 내용은 [ClickHouse 백업 문서](https://clickhouse.com/docs/en/operations/backup)를 참고하세요.

---

## VM / VPS 디스크 스냅샷

소규모 배포에 가장 간단한 방식입니다:

1. 컨테이너 중지: `docker compose down`
2. 클라우드 제공자 콘솔에서 전체 VM/VPS 디스크 스냅샷
3. 재시작: `docker compose --profile caddy up -d`

데이터베이스, 이벤트 스토어, 인증서 모두를 캡처합니다.

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
