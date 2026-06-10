# Architecture Decision Records

---

## ADR-001: Background Worker vs 외부 서비스

### 쟁점

Auto-processing (새 데이터 감지 → embedding → vector index 갱신) 같은 비동기 작업을
PostgreSQL Background Worker로 처리할 것인가, 외부 서비스 책임으로 넘길 것인가.

### 배경

EDB AIDB는 `aidb.set_auto_preparer()`를 Background Worker로 구현한다.
우리는 extension을 인터페이스 레이어로만 유지하는 것을 원칙으로 하고 있다.

### 옵션 A: Background Worker (EDB 방식)

extension 안에 background worker를 두고 변경 감지 + 처리를 담당한다.

```
PostgreSQL
  └── pg_aidb extension
        ├── SQL 함수
        └── Background Worker
              ├── 변경 감지 (shared memory / logical replication)
              └── 외부 서비스 HTTP 호출
```

**장점:**
- 변경 감지가 DB 내부에서 일어나므로 누락 없음
- 외부 인프라 의존성 없음 (extension만 설치하면 동작)
- PostgreSQL 트랜잭션과 동기화 가능

**단점:**
- Worker crash 시 PostgreSQL postmaster가 재시작 결정
- extension 복잡도 증가 (shared memory, latch 관리)
- 스케일 아웃 불가 (워커 수가 PostgreSQL 설정에 종속)
- 모니터링/디버깅이 DB 로그에 묻힘

---

### 옵션 B: Trigger + NOTIFY → 외부 Worker 서비스

extension은 트리거와 pg_notify만 설치하고,
실제 처리는 LISTEN하는 외부 서비스가 담당한다.

```
PostgreSQL
  └── pg_aidb extension
        ├── SQL 함수
        └── Trigger → pg_notify('aidb_pipeline', payload)

외부 서비스 (pipeline-worker)
  ├── LISTEN aidb_pipeline
  ├── 수신 → 처리 (embedding, indexing)
  └── 결과 → DB write
```

**장점:**
- Extension은 진짜 인터페이스만 (SQL + trigger)
- Worker 서비스 독립 스케일링, 재시작 가능
- 모니터링/트레이싱이 서비스 레벨에서 가능
- Worker 언어/프레임워크 자유롭게 선택

**단점:**
- pipeline-worker 서비스를 별도로 운영해야 함
- NOTIFY는 트랜잭션 커밋 시 발행되지만, worker가 다운된 사이의 이벤트 유실 가능
- PostgreSQL 커넥션을 LISTEN 용도로 상시 점유

---

### 옵션 C: Trigger + NOTIFY + 이벤트 큐 (Outbox Pattern)

NOTIFY 유실 문제를 outbox 테이블로 보완한다.

```sql
-- extension이 설치하는 outbox 테이블
CREATE TABLE aidb._outbox (
    id          bigserial PRIMARY KEY,
    event_type  text NOT NULL,
    payload     jsonb NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    processed_at timestamptz
);

-- 트리거: outbox 기록 + notify
CREATE FUNCTION aidb.notify_pipeline() RETURNS trigger AS $$
BEGIN
    INSERT INTO aidb._outbox (event_type, payload)
    VALUES ('row_inserted', row_to_json(NEW)::jsonb);
    PERFORM pg_notify('aidb_pipeline', NEW.id::text);
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
```

worker는 NOTIFY로 빠르게 반응하되, 재시작 후에는 outbox에서 미처리 이벤트를 복구한다.

**장점:**
- 이벤트 유실 없음 (outbox가 WAL에 기록됨)
- Worker 다운/재시작에 안전
- NOTIFY는 힌트 역할만 (없어도 outbox 폴링으로 복구)

**단점:**
- outbox 테이블 관리 (정리 job 필요)
- 구현 복잡도 증가

---

### 옵션 D: Logical Replication (CDC)

pg_aidb가 logical replication slot을 만들고,
외부 worker가 WAL을 직접 소비한다.

```
PostgreSQL WAL
  └── logical replication slot (aidb_slot)
        └── pipeline-worker (WAL 소비)
              ├── INSERT/UPDATE 감지
              └── 처리
```

**장점:**
- 가장 안정적. WAL 기반이라 이벤트 유실 원천 차단
- 트랜잭션 경계 정확히 반영
- 기존 Debezium/pglogical 생태계 활용 가능

**단점:**
- Replication slot 미소비 시 WAL 무한 증가 → 디스크 위험
- worker가 완전히 죽으면 slot 수동 삭제 필요
- 운영 복잡도 높음
- 일반 사용자에게 권한 부여 복잡

---

### 고려 사항

**배포 타겟이 무엇인가?**
- 단순 설치 (`CREATE EXTENSION`)만으로 동작해야 한다면 → 옵션 A
- Docker Compose / K8s 환경이 전제된다면 → 옵션 B, C, D

**이벤트 유실이 허용되는가?**
- 허용 안 됨 → 옵션 C (Outbox) 또는 D (CDC)
- 허용 가능 (재처리 트리거 있음) → 옵션 B

**운영 복잡도를 얼마나 감수할 수 있는가?**
- 낮게 유지 → 옵션 A 또는 B
- 복잡도 감수 가능 → 옵션 C 또는 D

---

### EDB AIDB는 왜 Background Worker를 썼는가

EDB AIDB는 두 가지 배포 모드를 지원한다.

- **Hybrid Manager 있음**: AI Factory 서비스들이 K8s에서 동작. extension은 인터페이스만.
- **Hybrid Manager 없음 (standalone)**: `CREATE EXTENSION aidb` 하나로 모든 게 동작해야 함. Background Worker가 auto-processing을 담당.

Background Worker를 extension에 넣은 이유는 **standalone 설치를 지원하기 위해서**다.
Hybrid Manager가 있으면 그 Worker는 사실상 놀게 된다.

우리는 standalone을 지원하지 않는다. Docker Compose가 최소 요구사항이므로
Background Worker를 extension에 둘 이유가 없다.

### 결론

**옵션 C (Trigger + NOTIFY + Outbox Pattern) 채택.**

- 배포 최소 요구사항: **Docker Compose** (standalone 미지원)
- Background Worker를 extension에 두지 않는다
- auto-processing 포함 모든 비동기 작업은 외부 서비스(pipeline-worker) 책임
- extension 역할: SQL 함수 + Trigger + NOTIFY + Outbox 테이블 설치까지

### 미결 사항

- [ ] 이벤트 유실 허용 여부 (Outbox로 충분한지, CDC가 필요한지)
- [ ] Outbox 테이블을 extension이 관리할 것인지, 사용자 스키마에 둘 것인지

---

## ADR-003: RAG 파이프라인 책임 분리 원칙

### 결론

**RAG 파이프라인은 최대한 외부 플랫폼 책임으로 만든다. Extension은 단순 호출만 한다.**

### 근거

SQL 인터페이스(`ai.retrieve()`, `ai.predict()` 등)를 호출하는 주체는 사람이 아니라 애플리케이션이다. 애플리케이션 입장에서 SQL 호출과 SDK 호출의 차이는 없다. SQL 인터페이스가 편리하다는 주장은 분석가가 직접 쿼리하는 케이스에만 유효하다.

RAG/LLM 워크로드는 DB, App, GPU, Storage가 각자의 전문성으로 독립적으로 스케일링되고 장애 도메인이 분리되어야 한다. DB 안으로 GPU 워크로드를 끌고 오면 스케일링 단위가 충돌하고 장애 도메인이 합쳐진다.

PostgresML은 in-process ML로 $4.7M을 받았으나 LLM 시대에 설계가 맞지 않아 2025년 서비스 종료했다. Oracle 26ai, EDB AIDB의 in-DB AI는 기술적 필요보다 벤더 락인에 가깝다.

### extension이 하는 것

- model_registry 조회
- 플랫폼 endpoint 호출 (pg_net)
- 결과 반환

### 플랫폼(외부 서비스)이 하는 것

- chunking
- embedding
- vector indexing
- retrieval
- reranking
- LLM 호출
- document ingestion
- pipeline orchestration

---

## ADR-004: 파이프라인 상태/아티팩트 가시성 딜레마

### 쟁점

단순 fire-and-forget이 아니라 파이프라인의 중간 상태와 아티팩트를 DB에서 유기적으로 관찰/제어할 수 있으면 가치가 있다. 그러나 이를 구현하는 방식에 따라 결합도가 달라진다.

### 옵션 A: 플랫폼이 DB 클라이언트 (강한 결합)

플랫폼 서비스가 PostgreSQL에 직접 접속해 상태와 아티팩트를 write한다.

```
ai.pipeline_runs   ← 잡 큐 + 상태 (extension 설치)
ai.chunks          ← 플랫폼이 직접 write
ai.embeddings      ← 플랫폼이 직접 write
ai.pipeline_signals ← 사용자 write, 플랫폼 read (제어)
```

```sql
-- 사용자가 SQL로 모든 것을 볼 수 있음
SELECT stage, status FROM ai.pipeline_runs WHERE id = $1;
SELECT * FROM ai.chunks WHERE run_id = $1;
INSERT INTO ai.pipeline_signals VALUES ($1, 'pause');
```

**장점:**
- 아티팩트와 상태를 SQL로 완전히 가시화
- 기존 데이터와 JOIN 가능
- 플랫폼이 stateless (재시작/스케일 아웃 자유)
- 재시도가 단순 (status 업데이트만으로 가능)

**단점:**
- 플랫폼이 PostgreSQL 스키마에 강하게 의존
- DB 스키마 변경 시 플랫폼 코드도 변경 필요
- 플랫폼 단위 테스트에 PostgreSQL 필요
- 플랫폼을 다른 시스템에서 재사용 불가

---

### 옵션 B: 플랫폼이 API 서버 (느슨한 결합)

플랫폼이 자체 상태를 관리하고, extension은 API 호출로만 상태를 조회한다.

```
extension → POST /pipeline/start     → 플랫폼
extension → GET  /pipeline/{id}/status → 플랫폼
extension → POST /pipeline/{id}/pause  → 플랫폼
```

**장점:**
- 플랫폼이 PostgreSQL에 독립적
- 플랫폼 내부 구현 자유롭게 변경 가능
- 플랫폼 단독 테스트 가능
- 다른 DB나 시스템에서도 플랫폼 재사용 가능

**단점:**
- 파이프라인 아티팩트가 DB에 없음 (`SELECT * FROM ai.chunks` 불가)
- 기존 데이터와 JOIN 불가
- 상태 조회마다 HTTP 호출 발생
- 플랫폼이 자체 상태 저장소 필요 (또 다른 DB)

---

### 딜레마의 본질

"파이프라인 아티팩트를 SQL로 보고 싶다"는 요구사항 자체가 결합을 요구한다.
아티팩트가 PostgreSQL에 있으려면 누군가 PostgreSQL에 써야 하고,
플랫폼이 쓰면 플랫폼이 PostgreSQL에 의존하고,
extension이 받아서 쓰면 extension이 아티팩트를 중계해야 한다.

```
SQL 가시성을 원한다  →  결합이 생긴다
결합을 피하고 싶다  →  SQL 가시성을 포기한다
```

### 옵션 C: 중계 (타협안)

플랫폼은 API 서버로 유지하되, extension이 플랫폼 완료 이벤트를 받아 아티팩트를 DB에 기록한다.

```
플랫폼  →  chunking 완료  →  NOTIFY 또는 webhook  →  extension이 수신
extension  →  ai.chunks에 write
```

**장점:**
- 플랫폼은 PostgreSQL 스키마에 무관
- 최종 아티팩트는 DB에서 조회 가능

**단점:**
- 중간 상태 가시성은 여전히 제한적
- extension이 아티팩트 수신/변환 로직을 가져야 함
- 플랫폼과 extension 간 이벤트 스키마 협약 필요

---

### 결론: 플랫폼 소유 상태 테이블 + Extension Read-Only Projection

플랫폼이 어차피 PostgreSQL을 주 저장소로 쓰는 구조이므로, 메인 상태 테이블은 플랫폼 소유로 두고 extension이 이를 바라보는 방식으로 딜레마를 해소한다.

**스키마 3층 구조:**

```
platform.*           ← 플랫폼 내부 구현 (extension이 직접 의존하지 않음)
platform_ai.*_v1     ← 플랫폼이 extension에 제공하는 versioned 계약면
ai.*                 ← 사용자에게 노출되는 제품면 (extension 소유)
```

플랫폼은 내부 테이블 구조를 자유롭게 바꾸되 `platform_ai.*_v1` 호환성 view만 유지한다.
extension은 원본 테이블이 아니라 이 계약면만 바라본다.

```sql
-- 플랫폼이 제공하는 계약면
CREATE VIEW platform_ai.pipeline_runs_v1 AS
SELECT id AS run_id, name AS pipeline_name, state AS status,
       progress, created_at, updated_at, finished_at AS completed_at,
       error_code, error_message
FROM platform.pipeline_runs;

-- extension이 사용자에게 노출하는 제품면
CREATE VIEW ai.pipeline_runs AS
SELECT * FROM platform_ai.pipeline_runs_v1;

CREATE FUNCTION ai.pipeline_status(p_run_id uuid) ...
    -- platform_ai.pipeline_runs_v1 조회
```

**별도 스트리밍 채널 불필요:**
- 상태 조회 → 테이블 직접 read (polling)
- 작업 트리거 → NOTIFY를 hint로만 사용 (없어도 polling으로 복구)
- 중간 상태 실시간성이 초 단위면 streaming 없이 충분

**소유권 정리:**
- 상태 전이 책임 → 플랫폼
- SQL 인터페이스 책임 → extension
- extension은 상태를 저장하지 않고 투영(projection)만 한다

---

## ADR-005: 공통 실행 모델 - Skill-Plan-Execute

### 결론

**Skill-Plan-Execute를 pg_aidb 플랫폼의 공통 실행 패턴으로 채택한다.**

### 층위 정의

세 층위는 역할이 명확히 다르다.

```
Skill   ← 문제 유형에 따른 상위 전략
            - 어떤 접근 방식을 쓸지 정의
            - 어떤 tool subset을 LLM에 노출할지 제한
            - LLM에게 줄 제약과 가이드 제공
            - tool list보다 메타적인 레이어

Plan    ← 선택된 skill 안에서 구체적 실행 계획 수립
            - 어떤 순서로, 어떤 리소스를, 어떻게 접근할지
            - 이 단계를 캐싱하면 지터 완화 가능
            - DSL을 쓴다면 Plan의 출력 포맷이 DSL이 됨

Execute ← 해당 skill이 허용한 tool로만 실행
            - 오류 시 Plan으로 피드백
            - 결과 검증 및 반환
```

### 각 서비스에서의 적용

**RAG:**
```
Skill   ← "semantic_search" / "hybrid_search" / "multi_hop_retrieval"
            → 어떤 인덱스 접근, reranking 여부, 탐색 홉 수
Plan    ← 어떤 청크를 어떤 순서로 검색할지
Execute ← vector search + rerank + 결과 조합
```

**Text2SQL:**
```
Skill   ← "simple_lookup" / "analytical" / "exploratory"
            → 허용 도구 subset, 접근 전략, 스키마 탐색 범위
Plan    ← 관련 테이블, 조인 관계, 집계 방식
Execute ← SQL 생성 + 실행 + 오류 시 재계획
```

**Inference:**
```
Skill   ← "classification" / "generation" / "embedding"
Plan    ← 모델 선택, 전처리 전략
Execute ← 실제 추론
```

### SQL 인터페이스

```sql
SELECT ai.execute(
    skill => 'semantic_search',
    input => '사용자 질문',
    config => '{"top_k": 10}'::jsonb
);

SELECT ai.execute(
    skill => 'text2sql_analytical',
    input => '지난달 매출 상위 10개 상품'
);
```

### Text2SQL 전략 쟁점 (미결)

Text2SQL의 Plan 단계 구현 전략이 확정되지 않았다.

**옵션 A: Query DSL**
- NL → DSL → SQL (결정론적 컴파일러)
- 지터 구조적 차단, DSL 설계/컴파일러 구현 비용 발생
- 복잡한 쿼리는 DSL 표현력에 제한

**옵션 B: MCP Tool 방식 (ClickHouse 전략)**
- LLM에게 `list_tables`, `run_query` 등 도구를 주고 에이전트로 탐색
- 유연하고 구현 단순, 지터 높음
- 멀티턴 스키마 탐색 가능

**옵션 C: 혼합**
- Skill이 문제 유형을 분류
- 단순/반복 쿼리 → DSL (캐싱, 지터 차단)
- 탐색적/복잡 쿼리 → MCP Tool 방식

- [ ] 타겟 유스케이스 확정 후 전략 결정

---

## ADR-006: Query-Time 블로킹과 이중 모드 API

### 쟁점

쿼리 타임에 임베딩/LLM/리랭킹 API를 호출할 때 PostgreSQL 커넥션 블로킹을 어떻게 처리할 것인가.

### 배경

PostgreSQL은 process-per-connection 모델이다. 하나의 백엔드 프로세스가 HTTP 응답을
기다리는 동안 해당 커넥션은 완전히 블록된다. AI 연산(임베딩 수백ms, LLM 수초~수십초)은
이 블로킹 시간이 길어 고동시성 환경에서 커넥션 풀 고갈로 이어진다.

**pg_net은 쿼리 타임 동기 결과에 해결책이 아니다.**

pg_net은 HTTP 요청을 BGW에 위임하고 즉시 request_id를 반환한다. 그러나 SQL 쿼리가
동기 결과를 반환해야 한다면, 결과가 올 때까지 커넥션을 붙들고 기다리거나 폴링해야 한다.
커넥션 점유 문제는 구조적으로 해결되지 않는다.

**pg_net이 유효한 용도:**
- 파이프라인 빌드(인제스천): fire-and-forget, 결과를 나중에 테이블에서 읽음
- 비동기 API: 즉시 request_id를 반환하고 결과를 나중에 수신

### Oracle 26ai / EDB aidb의 접근

- Oracle: in-process ONNX Runtime으로 쿼리 타임 임베딩 HTTP 제거. 그러나 LLM 호출은
  여전히 블로킹. 소형 ONNX 모델만 가능하며 외부 API 의존 구조에는 적용 불가.
- EDB aidb: 블로킹을 수용한다. 인제스천은 BGW 비동기, 쿼리 타임은 동기 HTTP.

두 제품 모두 고동시성 실시간 AI 서빙을 타겟하지 않는다. 분석/개발 워크로드가 주 타겟.

### 결론: 이중 모드 API

**동기 모드** — 개발, 분석, 저동시성 운영용

```sql
SELECT ai.search(query => '질문', pipeline => 'docs', top_k => 10);
SELECT ai.generate(query => '질문', pipeline => 'docs');
SELECT ai.embed('텍스트', 'openai');
```

- 커넥션 점유를 명시적으로 수용
- PgBouncer session mode + statement_timeout으로 완화
- 동기 결과가 필요한 모든 인터랙티브 쿼리에 사용

**비동기 모드** — 고동시성 프로덕션용

```sql
SELECT ai.search_async('질문', 'docs')    → request_id  -- 즉시 반환
SELECT ai.generate_async('질문', 'docs')  → request_id  -- 즉시 반환
SELECT ai.embed_async('텍스트', 'openai') → request_id  -- 즉시 반환
```

- 커넥션 즉시 반환
- BGW 또는 외부 워커(pipeline-worker)가 실제 처리
- 결과는 `ai.results` 테이블 + NOTIFY hint

### 결과 전달: NOTIFY는 힌트, 테이블이 진실

```sql
-- 완료 시 (BGW/워커가 수행)
INSERT INTO ai.results(id, status, data, finished_at) VALUES (...);
PERFORM pg_notify('ai_' || request_id::text, request_id::text);

-- 클라이언트 선택 A: NOTIFY (빠르지만 persistent 커넥션 필요)
LISTEN ai_<request_id>;

-- 클라이언트 선택 B: 폴링 (PgBouncer transaction mode 호환)
SELECT * FROM ai.results WHERE id = $1 AND status = 'done';
```

NOTIFY가 유실돼도 테이블 폴링으로 결과를 수신할 수 있다.
pg_aidb는 어느 방식을 쓸지 강제하지 않는다.

### LISTEN/NOTIFY 규모 한계

LISTEN/NOTIFY는 수백 커넥션 수준까지 실용적이다. 그 이상의 규모에서는:
- persistent 커넥션 유지 비용 → max_connections 재등장
- 브로드캐스트 thundering herd

수만 건 동시 알림이 필요하면 앞단에 Redis Pub/Sub 또는 WebSocket 서버가 필요하다.
이는 pg_aidb 범위 밖이며, 그 규모에서는 클라이언트가 폴링 방식으로 전환하면 된다.

### pg_net 사용 범위 정정

기존 ADR-003에서 "플랫폼 endpoint 호출 (pg_net)"으로 기술했으나, 이를 명확히 한다:

| 호출 유형 | 메커니즘 | 이유 |
|----------|---------|------|
| 동기 API (ai.search 등) | 직접 블로킹 HTTP | pg_net으로도 결과 대기 필요, 동일한 블로킹 |
| 비동기 API (ai.search_async 등) | pg_net fire-and-forget | 커넥션 즉시 반환 |
| 인제스천 파이프라인 | pipeline-worker (외부) | DB 커넥션 무관 |
| endpoint health check | BGW (직접 HTTP) | 이미 구현됨 |

---

## ADR-002: pg_aidb Extension vs Pure UDF

### 쟁점

pg_aidb를 컴파일된 extension (.so)으로 만들 것인가,
SQL 함수(UDF) + pg_net 조합으로만 구성할 것인가.

### 결론

**Extension으로 유지한다.**

이유:
- Background Worker 옵션을 열어두려면 extension이 필수
- GUC 변수 (`pg_aidb.vector_backend` 등) 등록은 extension만 가능
- Trigger 함수를 C/Rust로 작성하면 PL/pgSQL 대비 성능 우위
- 향후 pg_cuvs 연동 시 타입 시스템 통합 용이

PL/pgSQL UDF만으로 갈 경우 잃는 것:
- GUC 변수 (config 테이블로 대체 가능하나 우아하지 않음)
- Background Worker
- C 레벨 hook 가능성

---

## ADR-007: ingest/query 분리 시 공유 불변식과 단일 레지스트리

### 배경

B3(services 독립성 — RDB/VectorDB/Queue 추상화)을 추진하면 `ingest`(쓰기 경로)와
`search`/`ask`(읽기 경로)를 별도 서비스로 떼어내 독립 배포·스케일할 수 있게 된다.
이때 "두 경로를 아예 별개로 가도 되는가"라는 질문이 생긴다.

### 쟁점

`ingest`와 `search`/`ask`는 **독립적으로 호출 가능**하지만
**독립적으로 일관적이지는 않다**. 둘은 코드가 아니라 **데이터 계약(불변식)으로 결합**되어
있다. query가 옳게 동작하려면 ingest가 저장할 때와 동일한 임베딩 공간을 써야 한다.

이 불변식이 어긋나면 **에러 없이 조용히 엉뚱한 결과를 반환한다**(silent corruption).
이것이 "둘을 별개로 가면 상태가 크게 꼬인다"의 실체다.

### 공유 불변식

| 항목 | 양쪽 일치 필요? | 비고 |
|------|----------------|------|
| 임베딩 모델 | 필수 | query 벡터와 저장 벡터가 같은 모델이어야 유사도가 의미를 가짐 |
| 차원 (dimension) | 필수 | 불일치 시 연산 불가/무의미 |
| 거리 메트릭 | 필수 | cosine vs L2 |
| 컬렉션/파이프라인 정의 | 필수 | 검색 대상의 범위 |
| 청킹 방식 | 불필요 | write 시점에만 사용 — ingest/query가 달라도 무방 |

### 결론

**컴퓨트는 분리하되, 계약은 단일 레지스트리로 묶는다.**

1. **운영 독립은 허용** — ingest 서비스와 query 서비스를 별도 배포·독립 스케일하는 것은
   권장된다 (읽기/쓰기 부하 특성이 다름).
2. **레지스트리는 단일 공유 진실 원천** — `ai.pipelines` → `ai.models` → `ai.endpoints`가
   양쪽이 해석하는 유일한 출처여야 한다. 현재 `ai.ingest`와 `ai.search` 모두 `pipeline`
   인자를 받아 같은 레지스트리에서 `embed_model`을 해석하므로(`search_impl`이
   `lookup_endpoint(pipeline_cfg.embed_model)` 사용) 이 불변식이 이미 지켜지고 있다.
   레지스트리를 절대 fork 하지 않는다.
3. **백엔드 바인딩은 파이프라인 단위로** — 벡터 스토어/DB 백엔드를 pluggable하게 만들더라도,
   선택은 **서비스별 env(`VECTOR_BACKEND`)가 아니라 파이프라인 레지스트리 행에 저장**한다.
   서비스별 env로 두면 ingest는 pgvector에 쓰고 query는 Qdrant를 읽는 drift가 발생한다.
   양쪽이 같은 pipeline → 같은 store + 같은 model을 해석하게 한다.
4. **embed_model/dim 변경은 guarded 연산** — 기존 파이프라인의 임베딩 모델/차원을 바꾸는 것은
   "재인제스트 필요"를 강제하는 명시적 작업으로 만든다. 조용한 config edit으로 허용하지 않는다.
   (pipeline-worker는 이미 `ai.chunks` 차원과 `EMBED_DIMENSIONS` 불일치 시 fail loudly.)

### B3 추상화 계획에 대한 영향

기존 B3 계획을 폐기하지 않되, 위 원칙으로 재정렬한다:

- **B3.1 (스키마/테이블명 추상화)** — 그대로 유효, 위험 낮음. 먼저 진행.
- **B3.5 (standalone 마이그레이션)** — 레지스트리 fork 위험이 가장 큰 지점. standalone 모드도
  레지스트리 테이블을 반드시 부트스트랩하고, **배포당 레지스트리는 정확히 하나**임을 강제.
- **B3.3 (VectorStore plug-in)** — 가치 최고이자 위험 최고. **백엔드를 파이프라인 단위로
  바인딩**하도록 재설계(레지스트리 행에 backend + 연결 정보). 이것이 B3.3의 선결 설계 요소.
- **B3.4 (EventQueue 추상화)** — pgnotify는 outbox와 트랜잭션으로 묶여 있어, Redis/RabbitMQ로
  바꾸면 "outbox 행 기록 + notify"의 원자성을 잃는다. 기본은 pgnotify 유지, 큐 교체는
  고throughput 상황에서만. 우선순위 하향.
- **B3.2 (RDB 드라이버 추상화)** — 레지스트리 + results 큐는 트랜잭션 RDB를 요구. Postgres 외
  실수요 낮음. 인터페이스만 정의, 우선순위 최하.

권장 순서 갱신: **B3.1 → B3.5 (단일 레지스트리 강제) → B3.3 (파이프라인 단위 백엔드 바인딩) →
B3.4 → B3.2**.

### 트레이드오프 / 추가 결합

- **시간적 일관성** — ingest 완료 전 query 시 부분/빈 결과. `ai.results` status로 추적 가능
  (eventual consistency).
- **삭제/GC** — 컬렉션 삭제 시 chunks와 관련 상태가 함께 제거되어야 함.
- ADR-002에서 언급한 `pg_aidb.vector_backend` GUC는 이 ADR에 따라 **글로벌 GUC가 아니라
  파이프라인 단위 설정으로 재해석**된다.
