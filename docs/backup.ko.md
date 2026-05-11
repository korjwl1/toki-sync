# 백업과 복원

toki-sync의 상태는 두 개의 Docker 볼륨에 있습니다 — `toki-data`(메타데이터 + Fjall 이벤트)와 선택적인 `clickhouse-data`. 콜드 백업(서버 중지 후 백업)은 단순한 `tar` 아카이브로 충분합니다. 데이터를 잃어도 클라이언트가 로컬 이력을 보유하므로 재연결 시 자동 재동기화로 복구됩니다.

## 데이터 볼륨

| 볼륨 | 경로 | 내용 | 손실 시 |
|---|---|---|---|
| `toki-data` | `/data` | SQLite (사용자, 디바이스, 커서) + Fjall 이벤트 스토어 | 재로그인 + 전체 재동기화 필요 |
| `clickhouse-data` | `/var/lib/clickhouse` | ClickHouse 이벤트 데이터 (`--profile clickhouse` 사용 시) | 클라이언트 재동기화로 복구 가능 |
| `caddy-data` | `/data` | Let's Encrypt 인증서 | 자동 재발급 (Let's Encrypt 한도: 주당 5건의 중복 발급) |

기본 Fjall 백엔드에서는 `toki-data`에 메타데이터와 이벤트가 모두 포함됩니다. ClickHouse 사용 시 이벤트 데이터는 `clickhouse-data`에 별도로 저장됩니다. 손실되면 클라이언트가 재연결 시 로컬 이력에서 전체 재동기화를 수행합니다.

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

서버 실행 중에도 핫 백업으로 Fjall 디렉토리를 복사할 수 있습니다. Fjall은 LSM-tree 구조라 실행 중 복사가 안전하지만, 완전한 일관성은 서버를 중지해야만 보장됩니다.

---

## ClickHouse 백업 (선택적 백엔드)

ClickHouse를 이벤트 스토어로 사용하는 경우:

```bash
# clickhouse-backup 도구 사용
docker exec toki-clickhouse clickhouse-backup create backup_$(date +%Y%m%d)

# 또는 clickhouse-client로 내보내기
docker exec toki-clickhouse clickhouse-client \
  --query "SELECT * FROM events FORMAT Native" > events_backup.bin
```

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
