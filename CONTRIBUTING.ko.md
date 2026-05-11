# toki-sync 기여 가이드

기여에 관심을 가져주셔서 감사합니다. 이 문서는 프로젝트 셋업, 변경 작업 흐름 (브랜치, 커밋, PR), 코드가 통과해야 하는 스타일 검사를 다룹니다.

영문판은 [CONTRIBUTING.md](CONTRIBUTING.md)를 참고하세요.

## 개발 환경

### 사전 요구사항

- Rust 툴체인 (stable). 프로젝트는 Rust 2021 edition을 사용하며, 최신 stable 릴리스를 권장합니다.
- Docker 및 Docker Compose v2 (전체 컨테이너 스택을 로컬에서 실행할 때만 필요).

`Cargo.toml`에 `rust-version`은 명시되어 있지 않습니다. edition 2021을 지원하는 모든 stable 툴체인에서 빌드됩니다.

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

기여한 코드는 [FSL-1.1-Apache-2.0](LICENSE) 라이선스로 배포됨에 동의합니다.
