# pg_aidb — Backlog

최종 업데이트: 2026-05-20
현재 완료 상태:
- extension 코어 + 8 SQL 함수 + 53 pgrx 유닛 테스트
- RAG 플랫폼 (ingest / search / ask + async 3쌍) mock + real API 양쪽 E2E 통과
- platform_ai.*_v1 계약 뷰 7개, usage_v1로 토큰/비용 SQL 집계
- SECURITY 문서 + 옵트인 ACL, pg_dump round-trip 자동 검증, GitHub Actions CI
- 청커 4 메서드 디스패처, EMBED_DIMENSIONS env 추상화, langchain 완전 제거
- DEV_GUIDE 5분 quick start + PLAYBOOK 9 섹션 + README (pgai/Oracle/EDB 위치 정립)

상위 분류는 우선순위가 아니라 작업 도메인입니다. 각 항목 안에서만 우선순위 순서로 나열했습니다.

---

## A. RAG 깊이 (현재 플랫폼 강화)

### A1. opendataloader 파싱 경로 실증 검증 [DONE 2026-05-19]
- **결과**: PDF 파싱 mock + real API 양쪽 E2E 통과.
- **추가 성과**: 같은 작업에서 langchain 4종 (`experimental`, `openai`, `text-splitters`, `opendataloader-pdf` wrapper) 완전 제거. `services/shared/chunker.py`로 순수 Python 시맨틱 청킹 + 부모 청킹 자체 구현.
- **부가 산출물**: `tests/fixtures/sample.pdf` (1.7KB), `tests/fixtures/generate.py`, `extension/test/sql/rag_parse.sql`, `make run-rag-parse-{mock,real}` 타겟.
- **남은 작업**: HWP/DOCX/XLSX/PPTX는 별도 이터레이션 (포맷별 테스트 파일 + 검증).

### A2. `ai.search_async()` / `ai.ask_async()` 구현 (ADR-006) [DONE 2026-05-19]
- **결과**: 두 함수 모두 동작. uuid 즉시 반환 → pipeline-worker가 `event_type` 기반 dispatch → `ai.results.data`에 `results`(jsonb 배열) / `answer`(text) 저장.
- **부가 산출물**: `process_event()` 디스패처, `ai._outbox.event_type` 활용, `RAG_URL` env (pipeline-worker→rag), `rag_async.sql` E2E, 7개 유닛 테스트 추가 (43→50).
- **함정 (메모리에 저장)**: 테스트에서 DO 블록 안에 async 호출 + 폴링을 같이 넣으면 트랜잭션 격리로 worker가 INSERT를 못 봄. `COMMIT` 명시 필요.

### A2.5. 시맨틱 청커 품질 개선 [DONE 2026-05-19, 부분]
- **완료**: 메서드 디스패처 (`config.chunking.method`로 semantic/fixed/recursive/paragraph 선택), 마크다운 헤더+빈줄 분할자, threshold_type (percentile/stddev/interquartile), 기본 percentile 95→90, min/max chunk size 강제. extension의 `ingest_impl` 버그도 수정 (pipeline config jsonb가 outbox로 전달 안 되던 문제).
- **검증**: 동일 PDF 4 메서드 비교 — semantic 4청크, fixed 9, recursive 9, paragraph 20. 각각 의도대로 동작.
- **잔여**: (1) semantic의 마지막 작은 청크 merge가 max_chunk_size를 살짝 넘기는 엣지 케이스 (2084 > 2000), (2) buffer_size 옵션 (인접 문장 결합 후 임베딩), (3) 평가 데이터셋 + retrieval@k 측정. 별도 작업으로 분리.

### A3. Reranker 후처리 [BLOCKED 2026-05-19 — 한국어 옵션 평가 필요]
- **상태**: 미구현. 현재는 cosine similarity top-k만.
- **블로커**: 한국어 지원 reranker 후보 모두 trade-off가 큼
  - BGE-reranker-v2-m3 (multilingual, OSS) — 자체 호스팅 부담
  - Cohere rerank-multilingual-v3.0 — 유료, 벤더 추가
  - Qwen3-Reranker — 평가 부족
  - Korean fine-tune — 도메인 데이터 필요
- **재개 조건**: (1) 사용자 데이터셋으로 후보 평가, (2) 한국어 RAG 정확도 측정 가능한 골든셋 확보.
- **작업 (재개 시)**: rag 서비스 `/search`에 옵션 추가 (`use_reranker: bool`), `config.reranker` 스키마, pluggable provider (cohere/bge/none).

### A4. 멀티 모달 임베딩 (이미지)
- **상태**: 모델 레지스트리는 `model_type` 컬럼으로 이미 지원하지만 미구현.
- **작업**: `ai.embed_image(bytea)` 함수, CLIP 등 멀티모달 모델 등록, `ai.chunks`에 `modality` 컬럼 추가.
- **이유**: pgai에 없는 차별점. 우선순위 중.

### A5. 자동 소스 동기화 (pgai vectorizer 패턴) [NEW 2026-05-20]
- **동기**: 현재 `ai.ingest()`는 명시적 호출. pgai의 vectorizer는 source 테이블 CRUD를 자동 감지해 임베딩 갱신. 두 패턴 공존하면 사용자 선택 폭 확대.
- **설계**: `ai.attach_source(pipeline, table, columns)` → AFTER INSERT/UPDATE/DELETE 트리거 자동 설치 → trigger가 `ai.ingest()` 호출 (또는 직접 outbox INSERT).
- **고려**: 트리거 cascade · 삭제 동기화 (chunk 정리) · 변경 감지 효율 (timestamp/hash 기반 증분).

### A6. 메타데이터 필터링 [DONE 2026-05-22]
- **결과**: `ai.search(query, pipeline, top_k, filter jsonb DEFAULT '{}')` 추가. `metadata @> filter` (JSONB containment) 로 WHERE 조건 적용.
- **전파**: `ai.search`, `ai.ask`, `ai.search_hybrid` 모두 filter 파라미터 지원.
- **검증**: rag_filter.sql E2E — category=database/python 필터 3개 assertion 통과.
- **부가 발견**: 벡터 검색은 threshold 없어서 "0개 반환" 단언 불가 — 필터 정확성은 "반환된 결과가 모두 기대한 category인지"로 검증해야 함.

### A7. 하이브리드 검색 (BM25 + dense + RRF) [DONE 2026-05-20]
- **결과**: `ai.search_hybrid(query, pipeline, top_k, rrf_k, filter)` — textsearch_ko(MeCab) + pg_textsearch(BM25 bm25 인덱스) + pgvector HNSW를 RRF로 결합.
- **구현**: `ai.chunks.content_tsv tsvector GENERATED` + GIN 인덱스 + `ai_chunks_bm25_idx`. `ai.search_hybrid`는 순수 PL/pgSQL (schema.sql).
- **인프라**: textsearch_ko + pg_textsearch를 deploy/docker/extensions에 vendor, Dockerfile.pg에서 MeCab + USE_PGXS=1 빌드. `shared_preload_libraries=pg_textsearch` (docker-compose command).
- **검증**: rag_hybrid.sql E2E 3개 assertion 통과 (content_tsv 컬럼, function 존재, 결과 반환).
- **부가 발견**: pg_textsearch `<@>` 연산자는 window함수 ORDER BY에서 planner rewrite 안 됨 → 명시적 `to_bm25query(query, 'ai_chunks_bm25_idx')` 필요. pg named volume이 이미지 재빌드 후에도 옛 .so 잔류 → `docker compose down -v` 필수.

### A8. 결과 다양화 (MMR) [DONE 2026-05-22]
- **결과**: `ai.search_mmr(query, pipeline, top_k, fetch_k, lambda_param, filter)` — Maximal Marginal Relevance greedy 선택.
- **구현**: rag service `/search_mmr` 엔드포인트. `fetch_k` 후보를 pgvector에서 embedding 벡터째 가져와 Python numpy로 λ·sim(d,q)-(1-λ)·max_j sim(d,dj) 최대화. Rust: `call_search_mmr` + `search_mmr_impl` (9번째 pg_extern).
- **파라미터**: `fetch_k=20` (후보 풀), `lambda_param=0.5` (1=순수 relevance, 0=순수 diversity).
- **검증**: rag_mmr.sql — 3개 PostgreSQL 유사 문서 + 1개 Python 문서 → MMR top-3에 Python 문서 포함 (`mmr_includes_diverse_result=t`).

### A9. 컨텍스트 윈도우 관리 [DONE 2026-05-22]
- **결과**: `ai.ask(query, pipeline, top_k, max_context_tokens, strategy)` — `strategy='prune'`(기본) 또는 `'map_reduce'`.
- **prune**: 청크를 쌓다가 `max_context_tokens`(기본 3000) 초과 시 중단. 토큰 추정: `words×1.3`.
- **map_reduce**: 청크별 LLM 1-2줄 요약 → 요약본으로 최종 답변. N+1 LLM 호출.
- **변경**: Rust `ask_impl` + `call_ask` 파라미터 추가, Python `_apply_context_strategy` 헬퍼, `AskRequest` 확장.
- **검증**: rag_context.sql — prune/map_reduce 모두 `t`.

---

## B. 도메인 확장 (RAG 외 새 서비스)

### B1. `services/text2sql/` — 자연어 → SQL 번역
- **상태**: 디렉토리만 존재. `ai.text2sql()` 함수 미구현.
- **작업**:
  - Skill-Plan-Execute 패턴 (ADR-005)
  - 입력: 자연어 + 스키마 컨텍스트
  - 출력: 검증된 SQL 텍스트
  - 보안: 읽기 전용 보장, 권한 제한
- **미결정**: Query DSL vs MCP Tool 방식 (DECISIONS.md에 기록됨).

### B3. services 독립성 — RDB/VectorDB/Queue 추상화 [NEW 2026-05-19]
- **목표**: `pipeline-worker` + `rag`가 pg_aidb extension 없이도 standalone으로 동작. PostgreSQL/pgvector/LISTEN 외 다른 백엔드 지원.
- **동기**: 현재 services는 80% 독립이지만 `ai.*` 스키마 + `vector(1536)` + `NOTIFY aidb_pipeline`이 PostgreSQL/extension에 하드코딩됨. SaaS 형태로 분리 배포하려면 추상화 필요.

- **B3.1 스키마/테이블 이름 추상화** (1-2h) — 가장 작은 단계
  - `AI_SCHEMA=ai`, `RESULTS_TABLE=results`, `OUTBOX_TABLE=_outbox`, `DOCUMENTS_TABLE=documents`, `CHUNKS_TABLE=chunks` env vars
  - services/shared에 `tables.py` — 이름 빌더 (default = 현재 값)
  - 테스트: 다른 스키마명으로 한 사이클 실행
  - 산출물: extension의 DDL과 services의 DML이 같은 이름 변수로 묶임

- **B3.2 RDB 드라이버 추상화** (1일)
  - 인터페이스: `RDBClient` (`fetch_outbox`, `update_result`, `insert_chunks` …)
  - 구현: `PostgresRDBClient` (현재 psycopg3) + 미래 `MySQLRDBClient` / `SQLiteRDBClient` 자리
  - 드라이버 선택: `RDB_DRIVER=postgres` env
  - 단, 트랜잭션 / pgvector / SQL 방언 차이가 커서 1차 구현은 Postgres만 유지하고 인터페이스만 정의

- **B3.3 VectorDB 백엔드 plug-in** (1-2일) — 가장 큰 가치
  - 인터페이스: `VectorStore` (`upsert(id, vec, meta)`, `search(query_vec, top_k, filter)`, `delete_by_collection(name)`)
  - 구현: `PgvectorStore` (현재 동작) + 자리 `QdrantStore`, `PineconeStore`, `MilvusStore`
  - 선택: `VECTOR_BACKEND=pgvector|qdrant|pinecone` env
  - 모델별 차원: B3와 함께 `EMBED_DIMENSIONS` 환경변수화 (D2 부분 흡수)

- **B3.4 이벤트 큐 추상화** (0.5-1일)
  - 인터페이스: `EventQueue` (`publish(channel, payload)`, `subscribe(channel) -> AsyncIterator`)
  - 구현: `PgNotifyQueue` (현재) + 자리 `RedisStreamsQueue`, `RabbitMQQueue`
  - 선택: `EVENT_QUEUE=pgnotify|redis|rabbit` env
  - 트레이드오프: pgnotify는 transactional, redis/rabbit은 throughput 우수

- **B3.5 마이그레이션 분리** (1h)
  - extension 없이도 동작하도록 `services/migrations/postgres_standalone.sql` 추가
  - `ai._outbox`, `ai.results`, `ai.pipelines`, `ai.endpoints`, `ai.models`, `ai.documents`, `ai.chunks` 전부 직접 CREATE
  - extension 모드와 standalone 모드의 differing concerns 문서화

- **권장 순서**: B3.1 → B3.5 → B3.3 → B3.4 → B3.2
  - B3.1 + B3.5만으로도 "extension 없이 동작"은 달성됨 (~3h)
  - B3.3은 가장 큰 외부 가치 (pgvector 외 선택)
  - B3.2는 마지막에 — Postgres 외 RDB는 실수요 발생 시 평가

### B2. `services/inference/` — ONNX 런타임
- **상태**: 디렉토리만 존재. `ai.predict()` 미구현.
- **작업**:
  - ONNX Runtime FastAPI 래퍼
  - `ai.predict(model_name, input jsonb)` → jsonb
  - 모델 로딩/캐싱
- **이유**: 분류/회귀 등 임베딩 외 ML 사용 사례.

### B5. 멀티 프로바이더 지원 [DONE 2026-05-20]
- **결과**: 6개 프로바이더 지원 — OpenAI / Anthropic / Gemini / OpenRouter / Ollama / vLLM.
- **구현**: `LLM_PROVIDER` 디스패처(openai|anthropic) + Anthropic native adapter + OpenAI-compat 4종은 `OPENAI_BASE_URL`만으로 동작.
- **부가 발견**:
  - `EMBED_DIMENSIONS` 환경변수가 pipeline-worker + rag 양쪽에 다 있어야 함 (검색 시 query embed 차원 일치 필수)
  - Gemini gemini-embedding-001 기본 3072차원, pgvector HNSW 한계 2000이라 `dimensions=` 파라미터로 1536 축소 필수
  - Gemini OpenAI-compat 응답에 `usage` 필드 누락 — 가드로 처리됨
  - `OPENAI_BASE_URL`, `ANTHROPIC_BASE_URL` 모두 빈 문자열일 때 strip 필요 (SDK가 직접 env 읽음)
- **검증**: Gemini 2.5 Flash + gemini-embedding-001@1536으로 ingest/search/ask 전체 통과.

### B4. MCP 서버 — agent의 도구로 pg_aidb 노출 [NEW 2026-05-20]
- **동기**: README가 "에이전틱 워크로드는 DB 바깥에서"라고 주장. MCP는 그 구체 사례 — Claude Code/Cursor 같은 외부 에이전트가 pg_aidb의 검색·질의 함수를 도구로 호출.
- **작업**:
  - `services/mcp/` 신규 — `@modelcontextprotocol/server` 패턴
  - 노출 도구: `search(query, pipeline)`, `ask(query, pipeline)`, `list_pipelines()`, `usage_summary()`
  - 인증: API 토큰 또는 mutual TLS
- **이유**: 새 분배 채널. 시각적 데모가 강함. pgai에는 없음 (차별점).

---

## C. 운영 기반

### C1. `platform_ai.*_v1` 계약 뷰 (ADR-004) [DONE 2026-05-19]
- **결과**: 6개 뷰 — endpoints/models/pipelines/results (extension_sql) + documents/chunks (pipeline-worker schema.sql). api_key_env 숨김, endpoint_id 같은 내부 식별자 denormalize, results의 data jsonb를 stable 컬럼으로 투영.
- **연계**: restrict_acl.sql이 platform_ai 스키마 GRANT 사용. SECURITY.md 업데이트 필요(미반영).
- **확장 시**: 컬럼 추가는 자유, 제거/타입 변경은 v2 추가.

### C2. CI/CD — GitHub Actions [DONE 2026-05-19]
- **결과**: `.github/workflows/ci.yml` — PR + main push 시 자동 실행.
- **포함**: Docker 이미지 빌드 → pgrx 50 유닛 → mock RAG E2E → mock PDF parse E2E. 실패 시 pipeline-worker/rag/mock/pg 로그 자동 첨부.
- **잔여**: cargo + Docker layer 캐싱 (빌드 시간 단축), real API job (수동 트리거).

### C3. `design/DEV_GUIDE.md` [DONE 2026-05-19]
- **결과**: 5분 quick start (`make run-rag-mock`까지) + 일상 명령 + 구조 + 함정 10개 + 새 함수 추가 워크플로우 + CI. 일반 PostgreSQL extension 배경은 부록으로 유지.

### C4. 메트릭 / 로깅 [DONE 2026-05-19, lightweight]
- **결과**: 외부 인프라 0개로 운영 가시성 확보.
- **로그**: stdout JSON 라인 (services/shared/structured_log.py). docker compose logs로 바로 jq 가능.
- **메트릭**: usage 데이터를 `ai.results.data.usage`에 영속 → `platform_ai.usage_v1` 뷰로 op별 토큰/지연 집계.
- **확장 시**: Prometheus exporter는 필요해질 때 services에 추가 (현재는 SQL만으로 충분).

### C5. 보안 리뷰 — SECURITY DEFINER 함수 [DONE 2026-05-19]
- **결과**: `design/SECURITY.md` (위협 모델 + 감사 체크리스트), `extension/sql/restrict_acl.sql` (옵트인 PUBLIC REVOKE + `endpoints_public` 뷰).
- **핵심 발견**: api_key는 DB에 안 들어옴 (env var 이름만 저장). 함수는 SECURITY DEFINER + search_path 핀 되어 있어 escalation 위험 낮음. 멀티테넌트면 RLS + rate limit 추가 필요.

### C6. `pg_dump` / `pg_restore` 검증 [DONE 2026-05-19]
- **결과**: `make verify-dump-restore` — extension 설치 → 4개 테이블에 seed → pg_dump → fresh DB → restore → row count 검증 → cleanup, 전부 자동.
- **검증**: endpoints/models/pipelines/results 4개 다 round-trip 성공. `pg_extension_config_dump()` 호출이 실제로 작동함을 확인.

---

## D. 기술 부채

### D1. Pyright `# type: ignore` 없이 클린 빌드
- **상태**: 현재 0 errors. 단, `cast(LiteralString, ...)` 사용 중.
- **작업**: psycopg3 멀티스테이트먼트 처리를 더 깔끔하게 (statement 분할 또는 `executescript` 대안).

### D2. `ai.chunks.embedding` 차원 env 추상화 [DONE 2026-05-19]
- **결과**: `EMBED_DIMENSIONS` env (기본 1536). `init_schema()`가 schema.sql 템플릿의 `vector(1536)`을 런타임에 치환. 기존 테이블이 다른 차원이면 명시적 에러로 거부 (silent mismatch 방지).
- **확장**: text-embedding-3-large(3072) 사용 시 env만 변경 + 테이블 drop 한 번.

### D3. 빌더 컨테이너 dev 도구 [DONE 2026-05-19]
- **결과**: Dockerfile.builder에 jq, wget, procps, vim, less, tree 추가. 컨테이너 내 디버깅 시 답답함 해소.

### D4. `pyrightconfig.json` 중복 [DONE 2026-05-19]
- **결과**: `services/pyrightconfig.json` 삭제. 루트 하나만 유지 (Python 3.12, services/shared extraPaths).

### D5. `rag_mock.sql` 중복 + Makefile 대청소 [DONE 2026-05-19]
- **결과**: `rag_mock.sql` + `rag_mock.out` 삭제 (pg_regress 워크플로우 폐기). Makefile:
  - 죽은 타겟 제거 (`installcheck-rag-mock`, `installcheck-rag-real`, `promote-rag-mock`, `RAG_REGRESS_OPTS`)
  - `RAG_MOCK_TESTS` → `DIRECT_TESTS`로 의미 명확화 (rag_e2e/rag_parse/rag_async 다 옵트인)
  - 공통 bootstrap 로직을 `BOOTSTRAP_PG`/`WAIT_PIPELINE_WORKER`/`RUN_PSQL` define으로 통합
  - 모든 `run-rag-*` 타겟이 `DROP EXTENSION CASCADE`로 통일 (이전엔 일부 `DROP SCHEMA`로 깨졌음)
  - `pgrx-test` 타겟이 dev user + CARGO_TARGET_DIR=/home/dev/target 패턴으로 수정 (Unix socket 버그 회피)

---

## E. 미해결 디자인 질문

### E1. Text2SQL Plan 전략 (DECISIONS.md ADR-005)
- Query DSL 방식 (정형화된 중간 표현) vs MCP Tool 방식 (모델이 도구 호출) — 미결정.
- 결정 시점: B1 작업 착수 시.

### E2. Outbox vs CDC (DECISIONS.md ADR-001)
- 현재 Outbox + LISTEN/NOTIFY. CDC (logical replication) 도입 시점 미정.
- 결정 시점: NOTIFY 한계 (~500 동시 연결) 도달 시.

### E3. 청킹 전략의 사용자 노출 범위
- 현재 `config jsonb`에 `chunking`, `parent_child`, `breakpoint_threshold` 등을 자유롭게 받음.
- 검증/타입 안전성 vs 유연성 트레이드오프 — 사용 패턴 더 보고 결정.

---

## H. Memory layer 확장 (에이전트 메모리 인프라화)

**배경**: pg_aidb는 단순 "RAG 플랫폼"이 아니라 **에이전트 시스템의 메모리/검색 레이어**로 포지셔닝. RAG(semantic memory)는 그 한 종류일 뿐. 업계 흐름(Mem0, Zep, Qdrant)도 같은 방향.

### H1. Episodic memory — 대화/이벤트 시계열 [NEW 2026-05-20]
- **테이블**: `ai.episodes(id, session_id, actor, kind, content, embedding, ts, metadata)` — 시간 순 append-only.
- **함수**: `ai.remember(session, actor, content)`, `ai.recall(session, query, top_k)` — semantic + recency 결합.
- **이유**: 챗봇/어시스턴트가 "내가 어제 뭐 물어봤지" 같은 질의를 처리하려면 필수. RAG로는 불가능 (정적 문서 vs 동적 이벤트).

### H2. Working memory — session 단위 단기 컨텍스트 [NEW 2026-05-20]
- **테이블**: `ai.sessions(id, started_at, last_active_at, ttl_minutes, summary, state jsonb)`.
- **자동 만료**: TTL 기반, pipeline-worker가 주기적 정리.
- **요약 압축**: 세션 종료 시 LLM으로 요약 → episodes에 옮기고 working은 비움.
- **이유**: 에이전트 turn-by-turn 컨텍스트 관리. LLM 컨텍스트 윈도우 한계 대응.

### H3. 메모리 garbage collection / 압축 [NEW 2026-05-20]
- **문제**: 메모리는 무한히 커짐 → 검색 정확도/비용 악화.
- **작업**:
  - 시간 기반 decay (오래된 episodes는 weight 감소)
  - 유사 메모리 dedup (cosine > 0.95 시 병합)
  - 주기적 자동 요약 (10개 episode → 1 summary)
  - pipeline-worker에 `memory_maintenance` 이벤트 타입 추가
- **이유**: 장기 운영 시 필수. "기억"이 무한히 누적되면 인지 부담만 늘어남.

### H4. Retrieval 정책 — recency · importance · similarity 가중 [NEW 2026-05-20]
- **현재**: cosine similarity top_k만.
- **확장**: `ai.recall()`에 정책 jsonb — `{"weights":{"similarity":0.6,"recency":0.3,"importance":0.1}}`.
- **importance**: 사용자 또는 에이전트가 부여 (`ai.mark_important(memory_id, score)`).
- **이유**: 인간 메모리도 cue + recency + emotional weight 결합. 순수 similarity는 일부.

### H5. Memory 메타 — 사용/접근 통계 [NEW 2026-05-20]
- 어느 메모리가 자주 retrieve되나, 어느 것이 dead weight인가.
- `platform_ai.memory_stats_v1` 뷰 — access count, last_accessed, retrieval rank 평균.
- GC 정책의 입력 데이터.

---

## F. 입증 / 배포

### F1. 벤치마크 v1 (pg_aidb 자체) [DONE 2026-05-22]
- **결과**: `benchmarks/run.py` 스크립트 + `design/BENCHMARKS.md` 생성.
- **측정값** (OpenAI text-embedding-3-small, Colima/aarch64):
  - Ingest: **1141 docs/min**
  - Search p50: dense 228ms / hybrid 226ms / MMR 229ms (네트워크 지배)
  - Search p95: dense 285ms / hybrid 394ms (+109ms BM25 스캔)
  - Ask p50: **1.3s** (embed + search + LLM 전체 포함)
- **발견**: 대부분 latency는 OpenAI embed API 네트워크 시간. 로컬 embed (Ollama)로 전환 시 p50 ~10-30ms 예상.
- **F1 v2 (미완)**: pgai 동시 설치 + 동일 쿼리셋 비교 — 별도 작업.
- **비용**: 0.5일 + OpenAI 호출비

### F2. 데모 영상 (5분) [NEW 2026-05-20]
- PLAYBOOK 9 섹션을 그대로 따라 화면 녹화. 자막 + 짧은 보이스오버.
- 업로드 위치 미정 (YouTube / 자체 호스팅).
- **비용**: 2-3시간 (편집 포함).

### F3. README 영문 번역 [NEW 2026-05-20]
- 현재 한국어. GitHub 노출 시 영문 필수.
- README.md → README.ko.md, 신규 README.md(영문). DESIGN_PHILOSOPHY 등 핵심 문서도 점진 번역.
- **비용**: 0.5일.

### F4. 블로그 포스트 — "왜 RAG가 DB 안에 다 들어가면 안 되는가" [NEW 2026-05-20]
- DESIGN_PHILOSOPHY.md를 기반으로 외부 블로그용 글. process-per-connection + 슬로우 AI I/O 트레이드오프 + pgai/Oracle/EDB 비교.
- 차별화 포인트: 솔직함. PostgresML 중단 사례, Oracle 마케팅 vs 실제 한계.
- **비용**: 0.5-1일.

### F5. 한국어 RAG 평가 데이터셋 [NEW 2026-05-20]
- 한국어 골든셋 구축 (질문-정답 페어 50-200개). A3 reranker 블로커도 해결.
- 도메인: PostgreSQL/AI 관련 문서로 시작 (자기 점검 → 확장).
- HuggingFace 공개 → 외부 기여 유도.
- **비용**: 1-2일.

---

## G. 개발자 경험

### G1. CLI 도구 — `pg-aidb` 명령 [NEW 2026-05-20]
- psql 안 들어가도 `pg-aidb ingest /path/to/doc.pdf --pipeline=docs`, `pg-aidb ask "질문" --pipeline=docs` 같은 셸 명령.
- 구현: Python click 또는 typer, services/cli/ 신규.
- **비용**: 0.5일.

### G2. Python 클라이언트 SDK [NEW 2026-05-20]
- `pip install pg-aidb` — psycopg 래퍼 + 편의 함수.
- `client.ingest(...)`, `client.search(...)`, `client.ask(...)` + 자동 async 처리.
- **비용**: 0.5-1일.

### G3. 에러 메시지 정리 [NEW 2026-05-20]
- 현재 pgrx `error!()`는 raw Rust 패닉 → psql에서 stack trace 노출.
- HINT/DETAIL 추가, 사용자 친화 메시지로 통일.
- **비용**: 2-3시간.

---

## 우선순위 추천 (2026-05-20 기준)

원래 1-5번이 다 완료된 상태. 새 추천:

1. **A6 메타데이터 필터링** (0.5일) — 실사용에 거의 필수, 즉시 효용
2. **A7 하이브리드 검색** (1일) — reranker 블록 상태에서 정확도 개선 대안
3. **F1 pgai 실증 비교** (0.5일) — README가 추상에서 실측으로 격상
4. **A5 자동 소스 동기화** (1일) — pgai 패리티 + 명시 호출과 공존
5. **B4 MCP 서버** (1일) — 새 분배 채널, 시각적 데모
6. **B3.1 + B3.5 services 독립성 1차** (3h) — pgai-style standalone 옵션
7. **F3 README 영문** (0.5일) — 외부 공개 전 필수
8. **A4 멀티모달** (1-2일) — 차별점, pgai에 없음
9. **F2 데모 영상** (2-3h) — 인지도
10. **B2 inference (ONNX)** (1-2일) — 새 도메인

A1 잔여(HWP/DOCX 등), A2.5 잔여(평가셋), D1(Pyright 클린), C2 잔여(CI 캐싱), C4 잔여(Prometheus)는 필요 시 끼워넣는 식.
