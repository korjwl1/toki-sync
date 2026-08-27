# toki-sync 기여 가이드

기여에 관심을 가져주셔서 감사합니다. 이 문서는 프로젝트 셋업, 변경 작업 흐름 (브랜치, 커밋, PR), 코드가 통과해야 하는 스타일 검사를 다룹니다.

영문판은 [CONTRIBUTING.md](CONTRIBUTING.md)를 참고하세요.

## 개발 환경

### 사전 요구사항

- Rust 툴체인 (stable). 프로젝트는 Rust 2021 edition을 사용합니다. 현재 브랜치는
  Rust/Cargo 1.92.0에서 검증됐으며 그보다 낮은 MSRV는 보장하지 않습니다.
- Docker 및 Docker Compose v2 (전체 컨테이너 스택을 로컬에서 실행할 때만 필요).

`Cargo.toml`에 `rust-version`은 명시되어 있지 않습니다. 이는 Cargo가 MSRV를
강제하지 않는다는 뜻이지, edition 2021을 지원하는 모든 컴파일러에서 현재 의존성
그래프가 빌드된다는 뜻은 아닙니다.

`Cargo.toml`은 공개된 `toki-sync-protocol` v1.1.0 태그를 고정합니다. protocol 형제
체크아웃은 저장소 간 로컬 개발 때만 명시적으로 활성화하면 됩니다.

### 빌드 및 실행

```bash
cargo build
cargo test
```

로컬 SQLite + Fjall 스택으로 서버를 실행하려면 예시 설정을 복사하고 바이너리를 띄웁니다.

```bash
cp config/toki-sync.toml.example config/toki-sync.toml
cargo run -- --config config/toki-sync.toml
```

### 현재 검증 범위

커밋 `5804fc2`에서 `cargo test`는 116개 테스트를 나열합니다. 파서, protocol 프레임,
가격, HTTP 라우팅/인증 경로, 실제 임시 SQLite를 통한 monitor 설정, Fjall을
검사합니다. 현재 실제 PostgreSQL 또는 ClickHouse 통합 테스트는 없으며, 기존
ClickHouse 배포를 대상으로 한 마이그레이션 테스트도 없습니다. 단위 테스트 통과를
두 선택 백엔드의 검증으로 보고하면 안 됩니다.

v1.1.0 protocol 태그가 고정되었으므로 `docker build .`도 릴리즈 검증에 포함합니다.
위 PostgreSQL/ClickHouse 검증 범위는 그대로 적용됩니다. windows 기능을 소비하는
앱보다 toki-sync를 먼저 릴리즈합니다.

## Pull request

PR 하나에는 하나의 수정 또는 기능만 담아주세요. PR 열기 전에 다음을 확인합니다.

- 최신 `main`에 rebase합니다.
- `cargo fmt --all`과 `cargo clippy --all-targets --all-features -- -D warnings`를 실행합니다.
- `cargo test`를 실행합니다.
- 무엇이 어떻게 바뀌었는지 설명을 명확히 적습니다.

### 브랜치 이름

변경 종류 prefix + kebab-case 짧은 이름:

- `feat/<short-desc>` — 새 기능
- `fix/<short-desc>` — 버그 수정
- `docs/<short-desc>` — 문서 전용 변경
- `chore/<short-desc>` — 툴링, CI, 의존성 업데이트
- `refactor/<short-desc>` — 동작 변경 없는 구조 정리

### 커밋 메시지

이 저장소의 기존 이력과 동일한 [Conventional Commits](https://www.conventionalcommits.org/) prefix (`feat:`, `fix:`, `docs:`, `chore:`, `refactor:`, `test:`)를 사용합니다. subject는 72자 이내, 명령형으로 작성합니다 ("X 추가" 형식).

기존 이력 예시:

```text
feat: implement ClickHouse event store backend
fix: validate token_columns — length check + sanitize label names
docs: update deploy guides, config, .env for device code flow
```

### Sign-off (DCO)

별도 CLA는 없지만, 모든 커밋에 Developer Certificate of Origin sign-off가 필요합니다.

```bash
git commit -s -m "feat: add my change"
```

위 명령은 `Signed-off-by:` trailer를 추가하여 프로젝트 라이선스로 패치를 제출할 권한이 있음을 확인합니다.

## 이슈

버그를 발견했거나 기능 제안이 있으신가요? `.github/ISSUE_TEMPLATE/` 템플릿을 사용해 이슈를 열어주세요. 재현 절차, toki-sync 버전 (또는 커밋), 관련 로그를 포함합니다.

## 코드 스타일

- 커밋 전에 `cargo fmt --all`을 실행합니다.
- `cargo clippy --all-targets --all-features -- -D warnings`가 경고 없이 통과해야 합니다.
- 기존 코드베이스의 패턴을 따릅니다. 애매하면 인근 모듈의 구조를 우선 참고합니다.

## 라이선스

기여한 코드는 [MIT 라이선스](LICENSE)로 배포됨에 동의합니다.
