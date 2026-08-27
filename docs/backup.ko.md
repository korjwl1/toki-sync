# 백업과 복원

toki-sync 상태는 `toki-data`, 선택적 `clickhouse-data`, 선택적 `caddy-data`에
있습니다. 클라이언트에 로컬 이벤트 이력이 있어도 관계형 cursor가 남으면 event
store 손실이 자동 전체 업로드를 유발하지 않습니다. event store와 cursor DB를 한
복구 세트로 백업하세요.

## 데이터 볼륨

| 볼륨 | 경로 | 내용 | 손실 시 |
|---|---|---|---|
| `toki-data` | `/data` | SQLite (사용자, 디바이스, 커서) + Fjall 이벤트 스토어 | 재로그인 + 전체 재동기화 필요 |
| `clickhouse-data` | `/var/lib/clickhouse` | backend로 설정된 ClickHouse 이벤트/window | 관계형 cursor를 둔 채 빈 상태로 복원하면 cursor reset/전체 재동기화 필요 |
| `caddy-data` | `/data` | Caddy 내부 CA/인증서 데이터 | client trust를 다시 설정해야 할 수 있음 |

Fjall에서는 metadata/cursor와 event를 함께 복원하세요. ClickHouse에서는 같은 시점의
`toki-data`와 `clickhouse-data`를 복원하세요. ClickHouse profile만 시작해도
선택되지는 않으며 `[events].backend = "clickhouse"`가 필요합니다.

---

## Bind mount (백업에 권장)

백업 접근을 쉽게 하려면 `docker-compose.yml`에서 named volume 대신 bind mount를 사용하세요.

```yaml
volumes:
  - ./data/toki:/data
```

---

## `tar`로 콜드 백업

기본 Fjall 백엔드와 bind mount 수동 백업 모두 다음 절차로 동일하게 처리합니다.

> 예시는 `caddy` 프로파일을 기준으로 합니다. 본인 배포에 맞게 `up` 명령을 조정하세요.
> - 로컬 / 리버스 프록시: `docker compose up -d`
> - 자체 서명 (Caddy): `docker compose --profile caddy up -d`

```bash
# 일관성을 위해 컨테이너 중지
docker compose down

# 데이터 디렉토리 아카이브
tar czf toki-sync-backup-$(date +%Y%m%d).tar.gz ./data/

# 재시작 (배포 환경에 맞게 프로파일 조정)
docker compose --profile caddy up -d
```

이 저장소에는 SQLite/Fjall 또는 PostgreSQL/ClickHouse 전체를 포괄하는 검증된 hot
backup 절차가 없습니다. DB가 지원하는 coordinated snapshot을 사용하거나 cold
backup을 위해 writer를 중지하세요.

---

## ClickHouse 백업 (선택적 백엔드)

ClickHouse를 이벤트 스토어로 사용하는 경우:

```bash
# clickhouse-backup을 별도로 설치한 경우
docker exec toki-clickhouse clickhouse-backup create backup_$(date +%Y%m%d)

# 또는 두 테이블 내보내기
docker exec toki-clickhouse clickhouse-client \
  --query "SELECT * FROM toki_events FORMAT Native" > toki_events_backup.bin
docker exec toki-clickhouse clickhouse-client \
  --query "SELECT * FROM toki_windows FINAL FORMAT Native" > toki_windows_backup.bin
```

첫 명령 전에 `clickhouse-backup`을 확인/설치하세요. raw export에도 검증된 schema/data
restore 절차가 필요합니다.

자세한 내용은 [ClickHouse 백업 문서](https://clickhouse.com/docs/en/operations/backup)를 참고하세요.

---

## VM / VPS 디스크 스냅샷

소규모 배포에 가장 간단한 방식입니다.

1. 컨테이너 중지: `docker compose down`.
2. 클라우드 제공자 콘솔에서 전체 VM/VPS 디스크 스냅샷.
3. 재시작: `docker compose --profile caddy up -d` (배포 환경에 맞게 조정).

데이터베이스, 이벤트 스토어, 인증서가 모두 캡처됩니다.

---

## 복원

1. 컨테이너 중지: `docker compose down`.
2. 데이터 디렉토리를 백업으로 교체.
3. 재시작: `docker compose --profile caddy up -d` (배포 환경에 맞게 조정).
4. 클라이언트가 자동으로 재연결됩니다 (toki 데몬이 지수 백오프로 재시도).
