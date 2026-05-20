# pg_aidb

[![CI](https://github.com/ysys143/pg_aidb/actions/workflows/ci.yml/badge.svg)](https://github.com/ysys143/pg_aidb/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/ysys143/pg_aidb)](https://github.com/ysys143/pg_aidb/releases)

PostgreSQL용 in-DB AI 플랫폼. 임베딩 · 벡터 검색 · RAG를 SQL 함수로 노출하고, 무거운 컴퓨트는 외부 마이크로서비스로 분리한다.

```sql
SELECT ai.create_pipeline('docs', 'my-collection', 'default', 'default-llm', '{}');
SELECT ai.ingest('/data/paper.pdf', '', 'docs');
SELECT ai.ask('What is PostgreSQL?', 'docs');
```

---

## 왜 만들었나

데이터는 이미 PostgreSQL에 있다. 임베딩 · 검색 · 생성을 SQL 한 줄로 처리할 수 있다면 애플리케이션 복잡도가 줄어든다.

같은 카테고리의 상용/오픈소스 제품:
- **Oracle 23ai / 26ai** — `DBMS_VECTOR_CHAIN` 등 PL/SQL 패키지. in-process ONNX로 임베딩 블로킹 회피하지만 LLM은 여전히 외부 API 호출 = 블로킹. API 패턴(`UTL_TO_CHUNKS`, `GENERATE_TEXT`) 참고.
- **EnterpriseDB AIDB** — `aidb.retrieve()` / `aidb.retrieve_and_generate()`. Background Worker로 자동 처리. 검색·생성 함수 분리 패턴 참고.
- **PostgresML** — pgml extension. 모델 가중치를 DB 안에 두는 설계 — **2024년 이후 사실상 중단**. 이 방향은 의도적으로 피함.
- **Timescale pgai + vectorizer-worker** — DB는 큐·config·embedding storage만, vectorizer worker가 외부에서 인제스트 처리. DB 회사가 만든 제품인데 오히려 가장 절제된 설계로 좋은 참조 모델.

pg_aidb는 이 흐름에서 한 가지를 솔직히 인정하고 출발한다:

> PostgreSQL의 process-per-connection 모델 위에서 AI I/O(수백 ms ~ 수십 초)를 동기 함수로만 노출하면, 커넥션 풀이 곧 병목이다. (자세한 분석: `design/DESIGN_PHILOSOPHY.md`)

따라서 pg_aidb는 **SQL 인터페이스의 편의**와 **블로킹의 한계**를 분리해서 다룬다:
- 가벼운 호출(`ai.embed_raw`, `ai.search`): 동기 방식. 개발/분석용으로 충분.
- 무거운 호출(`ai.ingest`, `ai.ask`, `*_async`): NOTIFY + `ai.results` 폴링 방식이라 프로덕션에 안전.

---

## 포지셔닝 — RAG 플랫폼이 아니라 메모리 레이어

표층은 "PostgreSQL용 RAG 플랫폼"이지만 그건 출발점이고, 본질은 **에이전트 시스템의 검색/메모리 레이어**다.

에이전트 시스템에서 "DB가 필요하다"는 말은 거의 항상 다음을 동시에 요구한다:
- 벡터 유사도 검색 (semantic memory)
- 시계열 이벤트 저장 + 회상 (episodic memory)
- 세션 단위 단기 컨텍스트 (working memory)
- 자동 요약 · 압축 · garbage collection
- 사용 통계 기반 retrieval 정책

순수 DB로는 부족하고, 순수 앱 서비스로는 트랜잭션·백업·SQL이 약하다. **DB + 앱 로직 + 스토리지를 vertical하게 묶은 전문화된 서비스**가 정답. Qdrant/Pinecone/Weaviate가 "vector DB"에서 "agent memory infrastructure"로 포지셔닝을 옮기는 이유, Mem0/Zep가 처음부터 메모리 전용으로 출발하는 이유.

pg_aidb는 PostgreSQL의 성숙도 (트랜잭션·pg_dump·RLS·SQL 생태계)를 **재활용하면서** extension + services + Docker 한 패키지로 vertical 묶음을 제공한다. RAG는 그 위의 첫 사용 사례일 뿐.

| 메모리 종류 | 구현 상태 | 다음 |
|---|---|---|
| Semantic (참조 문서) | `ai.chunks` 구현됨 | (안정) |
| Episodic (대화/이벤트) | 미구현 | BACKLOG H1 |
| Working (session 단기) | 미구현 | BACKLOG H2 |
| GC/압축 | 미구현 | BACKLOG H3 |
| Retrieval 정책 (recency 등) | 미구현 | BACKLOG H4 |

---

## In-DB AI의 위상

**DB 안에 두는 것이 옳은 것들:**
| 항목 | 이유 |
|---|---|
| 임베딩 저장 + 벡터 유사도 검색 (pgvector) | 데이터 옆에 두는 게 명백히 효율적 |
| 결과 영속 + 감사 (`ai.results`) | 트랜잭션 일관성 |
| 메타데이터 레지스트리 (모델, 파이프라인) | 단일 진실 원천 |
| 비동기 작업 큐 (LISTEN/NOTIFY + Outbox) | 트랜잭션과 함께 commit |
| 안정된 계약 표면 (`platform_ai.*_v1`) | SQL 사용자 친화적 |

**DB 밖으로 빼야 옳은 것들:**
| 항목 | 이유 |
|---|---|
| 에이전트 루프 (도구 선택, 다단계 추론) | 백엔드 프로세스 점유 시간이 비결정적 |
| 모델 서빙 자체 | 메모리 · GPU · 격리 모두 DB와 무관 |
| 청킹 · 파싱 · OCR 같은 ETL | Python 생태계가 훨씬 풍부 |
| 복잡한 오케스트레이션 (Workflow, Saga) | DB는 오케스트레이터가 아니다 |
| 장시간 백그라운드 작업 | Background Worker도 결국 DB 프로세스 |

핵심 원칙은 에이전틱 워크로드는 DB 바깥에서 처리되어야 옳다는 것이다. DB는 상태 저장소 + 쿼리 엔진이지, 에이전트 호스트가 아니다. pg_aidb의 모든 무거운 컴퓨트는 외부 `pipeline-worker` / `rag` 서비스가 담당하고, DB는 큐 · 결과 영속 · SQL 인터페이스만 맡는다.

---

## 플랫폼 독자성 원칙

1. **Extension은 얇게** (ADR-003). 모든 무거운 컴퓨트는 외부 서비스. Extension 크래시 = DB 크래시는 절대 일어나면 안 됨.
2. **Dual-mode API** (ADR-006). `ai.search()`(sync) + `ai.search_async()`(uuid 반환 + 폴링). 사용자가 워크로드 특성에 따라 선택.
3. **Provider abstraction** (`services/shared/embedder.py`, `llm.py`). OpenAI → 다른 프로바이더 swap 시 한 곳만 수정.
4. **Pluggable chunking** (`chunker.py`). semantic / fixed / recursive / paragraph 4가지를 `config.chunking.method`로 선택.
5. **Stable contract** (`platform_ai.*_v1` views, ADR-004). 내부 `ai.*` 테이블을 자유롭게 리팩토링해도 외부 컨슈머는 안 깨짐.
6. **No vendor lock-in in DB**. API 키는 컨테이너 env에만 존재 (DB에는 env var 이름만). PostgresML 같은 "DB 안에 모델 가중치" 접근을 의도적으로 피함.
7. **Observability without infrastructure**. JSON stdout 로그 + `platform_ai.usage_v1` 뷰. 외부 Prometheus/Grafana 없이도 토큰 비용/지연 SQL로 집계 가능.

---

## 아키텍처

```
                  ┌─ ai.embed_raw / ai.search / ai.ask   (sync, blocking)
SQL ──── pg_aidb ─┤
                  └─ ai.ingest / *_async                  (async, NOTIFY)
                                  │
                                  ▼
                          ai._outbox  ◄── pipeline-worker (Python, LISTEN)
                                  │       ├─ opendataloader (PDF/DOCX/HWP)
                                  │       ├─ semantic chunker (no langchain)
                                  │       └─ OpenAI embed → pgvector store
                                  ▼
                          ai.results ◄── rag service (Python, HTTP)
                                          ├─ POST /search  → pgvector cosine
                                          └─ POST /ask     → GPT-4o-mini

contract surface: platform_ai.{endpoints,models,pipelines,documents,chunks,results,usage}_v1
```

| 컴포넌트 | 언어 | 책임 |
|---|---|---|
| extension/ | Rust (pgrx 0.18) | SQL 인터페이스 + HTTP 라우팅. 비즈니스 로직 없음. |
| services/pipeline-worker/ | Python (FastAPI) | LISTEN → 파싱 → 청킹 → 임베딩 → 저장 |
| services/rag/ | Python (FastAPI) | `/search` `/ask` `/v1/embeddings` HTTP API |
| services/shared/ | Python | embedder / llm / chunker / structured_log 추상 |
| services/text2sql/, services/inference/ | (예정) | 자연어→SQL, ONNX 추론 |

---

## 빠른 시작

```bash
cp .env.example .env
# OPENAI_API_KEY=sk-... 채우기

cd extension && make run-rag-real
# 한 줄로: 컨테이너 기동 → extension 설치 → 인제스트 → 검색 → 답변 → 정리
```

mock으로 비용 0:
```bash
cd extension && make run-rag-mock
```

수동 탐색 시나리오는 [`design/PLAYBOOK.md`](design/PLAYBOOK.md) 참조.

---

## 문서

| 파일 | 내용 |
|---|---|
| [design/ARCHITECTURE.md](design/ARCHITECTURE.md) | 전체 컴포넌트 + 데이터 흐름 |
| [design/DESIGN_PHILOSOPHY.md](design/DESIGN_PHILOSOPHY.md) | "왜 이렇게 설계했나" — 근본 제약과 결정 |
| [design/DECISIONS.md](design/DECISIONS.md) | ADR-001 ~ ADR-006 |
| [design/HANDOFF.md](design/HANDOFF.md) | pgrx 0.18 구현 패턴 + 함정 |
| [design/PLAYBOOK.md](design/PLAYBOOK.md) | 수동 테스트 시나리오 (9 섹션) |
| [design/DEV_GUIDE.md](design/DEV_GUIDE.md) | 5분 quick start + 함정 10개 |
| [design/SECURITY.md](design/SECURITY.md) | 위협 모델 + ACL 권장 |
| [design/BACKLOG.md](design/BACKLOG.md) | 완료/진행/대기 항목 |

---

## 비교 — 한 줄 정리

| 제품 | 모델 위치 | 인제스트 | 검색/생성 호출 | 워커 트리거 | 상태 |
|---|---|---|---|---|---|
| Oracle 23ai/26ai | in-process ONNX (임베딩) + 외부 LLM | sync, in-process | sync (블로킹) | — | 상용, 활발 |
| EDB AIDB | 외부 LLM | BGW | sync (블로킹) | BGW polling | 상용, 활발 |
| PostgresML | DB 안에 가중치 (pgml) | in-process | sync | — | 사실상 중단 |
| Timescale pgai | 외부 서비스 | vectorizer-worker (외부) | sync only (`ai.openai_chat_complete` 등) | work queue polling | 활발, 가장 절제 |
| **pg_aidb** | **외부 서비스** | **pipeline-worker** | **sync + async dual-mode** (`*_async`) | **LISTEN/NOTIFY + Outbox** | **개발 중** |

---

## 라이선스 / 기여

(추후)
