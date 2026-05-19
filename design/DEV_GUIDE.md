# pg_aidb 개발 가이드

5분 안에 첫 테스트가 돌아갑니다. 그 뒤 일상 작업 명령과 자주 마주치는 함정을 정리합니다.

---

## 1. 5분 Quick Start

### 사전 조건
- Docker Desktop (Mac/Linux)
- Git, Make
- `uv` (Python 패키지 매니저, `brew install uv`)
- OpenAI API 키 (real 테스트만 — mock은 불필요)

### 한 번만 (최초 클론 후)
```bash
git clone <repo> && cd pg_aidb

# 로컬 타입 체크용 (선택)
uv venv && uv pip install -r services/requirements-dev.txt

# 실제 API 테스트를 하려면 .env 작성
cp .env.example .env
# .env 열어서 OPENAI_API_KEY=sk-proj-... 채우기
# LLM_MODEL=gpt-5.4-mini (테스트 비용 최소화)
```

### 첫 테스트 — Mock 기반 (OpenAI 비용 0)
```bash
cd extension && make run-rag-mock
```
**기대 결과**: `E2E test complete — all assertions passed`

이 한 줄이 동작하면:
- Docker 컨테이너 5개 (pg + mock + pipeline-worker + rag + builder) 정상 기동
- Extension 빌드 + 설치
- Ingest → Search → Ask 전체 플로우

---

## 2. 일상 작업 명령

| 명령 | 용도 |
|---|---|
| `make run-rag-mock` | RAG E2E (offline, mock OpenAI) |
| `make run-rag-real` | RAG E2E (real API, .env 필요) |
| `make run-rag-parse-mock` | PDF 파싱 + ingest + search + ask (mock) |
| `make run-rag-parse-real` | 동일 (real) |
| `make run-rag-async-real` | search_async / ask_async 동작 검증 |
| `docker compose exec builder bash -c 'cd /workspace/extension && cargo pgrx test pg17'` | pgrx 50 유닛 테스트 |
| `.venv/bin/pyright services/...` | Python 타입 체크 (로컬 .venv 필요) |
| `cd tests/fixtures && ../../.venv/bin/python generate.py` | PDF 픽스처 재생성 |

### Rust 코드 변경 후
```bash
# 1) 컴파일 + pgrx install (Docker 내부)
docker compose exec builder bash -c "cd /workspace/extension && cargo pgrx install --pg-config /usr/bin/pg_config"

# 2) 기존 extension 제거 후 재설치
docker compose exec pg psql -U postgres -d aidb -c "DROP EXTENSION IF EXISTS pg_aidb CASCADE; CREATE EXTENSION pg_aidb;"

# 3) pipeline-worker 재시작 (ai 스키마 다시 만들어야 함)
docker compose restart pipeline-worker
```

### Python 서비스 변경 후
```bash
docker compose build pipeline-worker  # 또는 rag
docker compose up -d --force-recreate pipeline-worker  # 또는 rag
```

---

## 3. 프로젝트 구조

```
pg_aidb/
├── extension/                  Rust pgrx 확장
│   ├── src/
│   │   ├── lib.rs              ai schema + 테이블 DDL (extension_sql!)
│   │   ├── registry/mod.rs     lookup_endpoint, lookup_pipeline
│   │   ├── client/mod.rs       reqwest blocking HTTP
│   │   └── router/mod.rs       #[pg_extern] 함수들 (embed_raw, ingest, search, ask, *_async)
│   ├── test/sql/               pg_regress + psql 직접 실행용 E2E SQL
│   └── Makefile                run-rag-* 타겟들
├── services/
│   ├── shared/                 두 서비스 공유 모듈
│   │   ├── embedder.py         embed(texts) - OpenAI embeddings 추상화
│   │   ├── llm.py              generate(messages) - chat completion 추상화
│   │   └── chunker.py          chunk(text, method, ...) - 4가지 청킹 전략
│   ├── pipeline-worker/        LISTEN on aidb_pipeline → ingest/search/ask 처리
│   └── rag/                    /search /ask /v1/embeddings HTTP API
├── tests/fixtures/             sample.pdf + generate.py
├── design/                     ARCHITECTURE, DECISIONS, HANDOFF, BACKLOG, DEV_GUIDE
├── deploy/docker/              Dockerfile.pg/.builder/.pipeline-worker/.rag/.mock
└── docker-compose.yml          전체 스택
```

핵심 추상화:
- **모델 레지스트리**: `ai.endpoints` + `ai.models` + `ai.pipelines` — 한 곳에서 변경
- **shared abstraction**: `embedder.py` / `llm.py` — provider 교체 시 여기만
- **NOTIFY/Outbox**: `ai._outbox` + `aidb_pipeline` 채널 — 비동기 작업의 단일 큐

---

## 4. 자주 마주치는 함정

### A. `make run-rag-*`이 동작하지 않을 때
```bash
docker compose ps
# pg / pipeline-worker / rag 모두 (healthy) 인지 확인
docker compose logs pipeline-worker --tail=30
```

### B. `cargo pgrx test pg17`이 SIGKILL/메모리 부족
Docker Desktop 메모리 8GB 이상 권장. 재시도하면 캐시로 빨라짐.

### C. `cargo pgrx test pg17`이 Unix 소켓 오류
**원인**: macOS Docker 마운트 볼륨에서 Unix 소켓 생성 불가  
**해결**: 항상 `dev` 유저 + `CARGO_TARGET_DIR=/home/dev/target` (Makefile `pgrx-test`는 이미 적용)

### D. `OPENAI_API_KEY: ${OPENAI_API_KEY:-}` 빈 문자열 → `APIConnectionError`
**원인**: 빈 문자열도 SDK가 base_url로 사용  
**해결**: `services/pipeline-worker/src/main.py`와 `rag/src/main.py` 최상단에서 `del os.environ["OPENAI_BASE_URL"]` 처리 — 이미 적용됨

### E. `CREATE EXTENSION IF NOT EXISTS pg_aidb`가 옛 버전 그대로 둠
**원인**: `cargo pgrx install`은 파일만 복사. `IF NOT EXISTS`는 기존 extension 그대로 둠.  
**해결**: 항상 `DROP EXTENSION IF EXISTS pg_aidb CASCADE` 먼저 → `CREATE EXTENSION pg_aidb`

### F. `schema ai is not a member of extension`
**원인**: pipeline-worker가 `CREATE SCHEMA ai`로 먼저 만들었거나 이전 테스트 잔여  
**해결**: `DROP EXTENSION IF EXISTS pg_aidb CASCADE` (스키마도 같이 정리됨) → pipeline-worker 재시작

### G. DO 블록 안에서 async kickoff + polling이 안 끝남
**원인**: DO 블록은 하나의 트랜잭션. INSERT가 commit 안 되면 pipeline-worker가 못 봄.  
**해결**: kickoff 직후 `COMMIT;` 명시 (PostgreSQL 11+ 지원). `rag_async.sql` 참고.

### H. pg_regress가 로컬 PostgreSQL을 찾으려 함
**원인**: Mac에 PostgreSQL 미설치 + pg_regress가 Unix 소켓으로 접속 시도  
**해결**: 항상 `docker compose exec builder` 안에서 실행 (PGHOST=pg 등 env 자동)

### I. RAG E2E가 pg_regress와 충돌
**원인**: pg_regress는 `contrib_regression` DB를 새로 만듦. pipeline-worker는 `aidb` DB만 LISTEN. NOTIFY는 DB 단위.  
**해결**: RAG E2E는 pg_regress 대신 `psql -f rag_e2e.sql` 직접 실행 (`make run-rag-real` 사용)

### J. psql `:'var'`이 DO 블록 안에서 치환 안 됨
**원인**: psql 치환은 $$...$$ 안을 안 건드림  
**해결**: PL/pgSQL `DECLARE`로 함수 반환값 받기 — `rag_async.sql` 참고

---

## 5. 새 SQL 함수 추가 워크플로우

`/rag-add-function` 스킬 사용 또는 `.claude/skills/rag-add-function/SKILL.md` 참고.

요점:
1. `extension/src/router/mod.rs` — `#[pg_extern]` 함수 추가
2. `ALTER FUNCTION ... SET search_path` extension_sql! 블록 업데이트
3. 필요 시 `client/mod.rs`에 HTTP 호출 추가
4. async라면 pipeline-worker에 `process_<name>` 핸들러 추가
5. 단위 테스트 4개 이상 (happy path, panic on missing, side effect 검증)
6. `cargo pgrx test pg17` → 모두 통과
7. `make run-rag-mock` → 회귀 없음

---

## 6. 모델 프로바이더 설정 매트릭스

`.env`에 아래 조합 중 하나를 채우면 됩니다.

### OpenAI (기본)
```bash
LLM_PROVIDER=openai
OPENAI_API_KEY=sk-proj-...
EMBED_MODEL=text-embedding-3-small
LLM_MODEL=gpt-5.4-mini
```

### Anthropic (Claude) — LLM only, 임베딩은 별도 OpenAI-compat 필요
```bash
LLM_PROVIDER=anthropic
ANTHROPIC_API_KEY=sk-ant-...
LLM_MODEL=claude-sonnet-4-6
# 임베딩은 여전히 OpenAI/Gemini/Cohere 등 OpenAI-compat 사용
OPENAI_API_KEY=sk-proj-...
EMBED_MODEL=text-embedding-3-small
```

### Gemini (OpenAI 호환 모드)
```bash
LLM_PROVIDER=openai
OPENAI_API_KEY=<Google AI API key>
OPENAI_BASE_URL=https://generativelanguage.googleapis.com/v1beta/openai/
LLM_MODEL=gemini-3.1-flash-lite-preview
EMBED_MODEL=gemini-embedding-001
EMBED_DIMENSIONS=1536
```

### OpenRouter — 한 키로 다 호출
```bash
LLM_PROVIDER=openai
OPENAI_API_KEY=sk-or-v1-...
OPENAI_BASE_URL=https://openrouter.ai/api/v1/
LLM_MODEL=anthropic/claude-sonnet-4-6
EMBED_MODEL=openai/text-embedding-3-small  # 또는 voyage 등
```

### Ollama (로컬 모델)
```bash
LLM_PROVIDER=openai
OPENAI_API_KEY=ollama        # 아무 문자열
OPENAI_BASE_URL=http://host.docker.internal:11434/v1/
LLM_MODEL=qwen3:0.6b
EMBED_MODEL=bona/bge-m3-korean:latest
EMBED_DIMENSIONS=1024
```

### vLLM (자체 호스팅)
```bash
LLM_PROVIDER=openai
OPENAI_API_KEY=any
OPENAI_BASE_URL=http://your-vllm:8000/v1/
LLM_MODEL=meta-llama/Llama-3.2-3B-Instruct
```

핵심: **OpenAI-compatible 엔드포인트는 코드 변경 없이 env만으로 동작**. Anthropic만 별도 native SDK 어댑터.

---

## 7. CI

`.github/workflows/ci.yml` — PR마다 자동 실행:
- pgrx 50 유닛 테스트
- `make run-rag-mock` (RAG E2E)
- `make run-rag-parse-mock` (PDF 파싱 E2E)

실패 시 pipeline-worker / rag / mock 로그가 자동 첨부됨.

---

## 7. 부록 — PostgreSQL Extension 일반 배경

(아래는 새 프로젝트 시작 시 참고 — pg_aidb 작업에는 위 섹션만 필요)

### 왜 Linux 환경이 필요한가
PostgreSQL extension 빌드는 `pg_config`가 반환하는 경로와 플래그에 의존합니다. Mac의 `pg_config`와 Linux의 `pg_config`는 컴파일러/링커 플래그가 다르기 때문에, Linux 타겟 배포라면 반드시 Linux 환경에서 빌드해야 합니다.

### 언어 선택: C vs Rust vs Python

| 항목 | C | Rust | Python (PL/Python) |
|------|---|------|--------------------|
| 성능 | 최고 | C와 동급 | 낮음 |
| 안전성 | 낮음 | 높음 | 높음 |
| 개발 속도 | 느림 | 중간 | 빠름 |
| 빌드 복잡도 | 중간 (PGXS) | 낮음 (cargo-pgrx) | 없음 |
| 내부 API 접근 | 완전 | 완전 | 제한적 |

pg_aidb는 **Rust + pgrx**를 사용합니다 (cargo-pgrx 0.18). 새 프로젝트라면 동일 스택을 권장.

### pgrx 기본
```bash
cargo install cargo-pgrx
cargo pgrx init           # PG 버전별 로컬 설치
cargo pgrx new my_ext     # 새 프로젝트
cargo pgrx run            # 개발 서버
cargo pgrx test           # 테스트
cargo pgrx package        # 배포용
```

### 배포 산출물
```
/usr/lib/postgresql/17/lib/pg_aidb.so          # 공유 라이브러리
/usr/share/postgresql/17/extension/
    pg_aidb.control                             # 메타데이터
    pg_aidb--0.1.0.sql                          # SQL 정의
```

### 참고
- [PostgreSQL Extension 공식 문서](https://www.postgresql.org/docs/current/extend-extensions.html)
- [pgrx GitHub](https://github.com/pgcentralfoundation/pgrx)
- 프로젝트 내부: `design/ARCHITECTURE.md`, `design/DECISIONS.md`, `design/HANDOFF.md`
