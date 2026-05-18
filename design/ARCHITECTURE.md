# PostgreSQL AI Extension - Architecture

## 프로젝트 구조

두 개의 독립 프로젝트로 분리한다.

| 프로젝트 | 언어 | 역할 |
|----------|------|------|
| **pg_aidb** | Rust (pgrx) + Python (FastAPI) | AI/RAG 기능 통합 extension + MSA 서비스 |
| **pg_cuvs** | C++ + CUDA | libcuvs 기반 GPU 벡터 검색 extension |

pg_aidb는 벡터 검색 백엔드로 pg_cuvs를 선택적으로 사용한다.
pg_cuvs는 pg_aidb 없이도 독립적으로 동작한다.

---

# 1. pg_aidb

## 시스템 구성

```
┌─────────────────────────────────────────────────────────┐
│                    Client Application                    │
└─────────────────────┬───────────────────────────────────┘
                      │ SQL
┌─────────────────────▼───────────────────────────────────┐
│                PostgreSQL + pg_aidb                      │
│                                                         │
│  ai.execute(skill, input)   ai.predict()                │
│  ai.nl2sql()  ai.ingest()   ai.vector_search()          │
│                                                         │
│  ┌──────────────────┐  ┌──────────────────────────┐    │
│  │  model_registry  │  │   platform state views   │    │
│  │  ai.models       │  │   ai.pipeline_runs (view)│    │
│  │  ai.endpoints    │  │   ai.pipeline_status()   │    │
│  └────────┬─────────┘  └──────────────────────────┘    │
│           │ HTTP (pg_net)                               │
└───────────┼─────────────────────────────────────────────┘
            │
   ┌────────┴────────┬──────────────┬──────────────┐
   ▼                 ▼              ▼              ▼
┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────────┐
│inference │  │  nl2sql  │  │   rag    │  │  pipeline    │
│  :8001   │  │  :8003   │  │  :8002   │  │  worker      │
│          │  │          │  │          │  │              │
│ ONNX     │  │ Skill-   │  │ Skill-   │  │ chunking     │
│ runtime  │  │ Plan-    │  │ Plan-    │  │ indexing     │
│          │  │ Execute  │  │ Execute  │  │ (async)      │
└──────────┘  └──────────┘  └──────────┘  └──────────────┘

  벡터 검색 백엔드 (pg_aidb.vector_backend로 선택)
  ┌──────────────────────┬──────────────────────┐
  │ pgvector (기본값)     │ pg_cuvs (GPU 서버)   │
  └──────────────────────┴──────────────────────┘
```

## 설계 원칙

Extension은 얇게 유지한다. 카탈로그 관리와 요청 라우팅만 담당하고,
무거운 연산(ONNX 추론, LLM 호출, 문서 파싱)은 모두 외부 서비스로 분리한다.

- Extension crash = PostgreSQL crash. 불안정한 연산을 in-process에 두지 않는다.
- 서비스별 독립 스케일링 (GPU 서버, CPU 서버 분리 배치 가능)
- 모델/서비스 교체 시 DB 재시작 불필요
- RAG 파이프라인은 최대한 외부 플랫폼 책임. extension은 호출과 투영만 한다.

## 공통 실행 모델: Skill-Plan-Execute

모든 AI 요청은 Skill-Plan-Execute 패턴으로 처리한다.

```
Skill   ← 문제 유형에 따른 상위 전략
            - 어떤 접근 방식을 쓸지 정의
            - LLM에 노출할 tool subset 제한
            - tool list보다 메타적인 레이어

Plan    ← 선택된 skill 안에서 구체적 실행 계획 수립
            - 캐싱 가능 → 지터 완화

Execute ← skill이 허용한 tool로만 실행
            - 오류 시 Plan으로 피드백
```

```sql
SELECT ai.execute(skill => 'semantic_search', input => '사용자 질문');
SELECT ai.execute(skill => 'nl2sql_analytical', input => '지난달 매출 상위 10개');
SELECT ai.execute(skill => 'hybrid_search', input => '...', config => '{"top_k":10}'::jsonb);
```

## 언어

| 컴포넌트 | 언어 | 이유 |
|----------|------|------|
| extension | Rust (pgrx) | in-process 안전성, C 수준 성능 |
| inference-service | Python (FastAPI) | onnxruntime 공식 SDK |
| nl2sql-service | Python (FastAPI) | schema inspection + LLM (OpenAI-compatible) |
| rag-service | Python (FastAPI) | embedding, retrieval, reranking |
| pipeline-worker | Python | chunking, indexing, document ingestion (async) |

## model_registry - 단일 진실 원천

모든 모델과 서비스 endpoint를 여기서 관리한다.
어떤 기능이든 model_registry를 통해 모델을 조회하고 라우팅한다.
LLM provider는 추상화되며 model_registry의 endpoint 설정으로만 구분한다.

```sql
CREATE TABLE ai.models (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name        text UNIQUE NOT NULL,
    version     text NOT NULL DEFAULT '1.0',
    model_type  text NOT NULL,        -- 'onnx' | 'llm' | 'embedding' | 'reranker'
    provider    text NOT NULL,        -- 'openai-compat' | 'vllm' | 'ollama' | 'local'
    endpoint_id uuid REFERENCES ai.endpoints(id),
    config      jsonb NOT NULL DEFAULT '{}',
    is_default  boolean NOT NULL DEFAULT false,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE ai.endpoints (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name        text UNIQUE NOT NULL,
    service     text NOT NULL,        -- 'inference' | 'nl2sql' | 'rag' | 'pipeline'
    base_url    text NOT NULL,
    api_key_env text,                 -- 환경변수 이름 (값 직접 저장 금지)
    health_url  text GENERATED ALWAYS AS (base_url || '/health') STORED,
    is_active   boolean NOT NULL DEFAULT true,
    last_checked_at timestamptz
);

CREATE TABLE ai.model_versions (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    model_id     uuid NOT NULL REFERENCES ai.models(id),
    version      text NOT NULL,
    artifact_uri text,                -- s3://, gs://, /local/path
    metadata     jsonb NOT NULL DEFAULT '{}',
    promoted_at  timestamptz,
    UNIQUE (model_id, version)
);
```

## 플랫폼 상태 가시성

플랫폼이 PostgreSQL을 주 저장소로 사용한다. 상태 테이블은 플랫폼 소유이며
extension은 read-only projection만 제공한다.

```
platform.*           ← 플랫폼 내부 구현
platform_ai.*_v1     ← 플랫폼이 extension에 제공하는 versioned 계약면
ai.*                 ← 사용자에게 노출되는 제품면 (extension 소유)
```

```sql
-- 파이프라인 상태 조회 (extension이 노출하는 view)
SELECT * FROM ai.pipeline_runs WHERE status = 'running';
SELECT * FROM ai.pipeline_status($run_id);

-- 파이프라인 제어
INSERT INTO ai.pipeline_signals VALUES ($run_id, 'pause');
```

상태 조회는 테이블 직접 read (polling). 작업 트리거는 NOTIFY를 hint로만 사용한다.

## 서비스 API 규약

### 공통

```
GET  /health  ->  { "status": "ok", "version": "x.y.z" }
GET  /models  ->  { "models": [...] }
```

### inference-service (:8001)

```
POST /predict
     <- { "model_name": str, "input": any, "config": {} }
     -> { "output": any, "model": str, "latency_ms": int }

POST /predict/batch
     <- { "model_name": str, "inputs": [any], "config": {} }
     -> { "outputs": [any], "model": str, "latency_ms": int }
```

### nl2sql-service (:8003)

Skill-Plan-Execute 패턴으로 처리. 모든 LLM은 OpenAI-compatible API로 추상화.

```
POST /execute
     <- { "skill": str, "input": str, "model_name": str, "config": {} }
     -> { "sql": str, "plan": {}, "explanation": str }

GET  /skills  ->  등록된 skill 목록
```

nl2sql-service 내부 컴포넌트:
- schema inspector (pg_catalog 조회)
- schema RAG (관련 테이블 선별)
- skill registry (skill별 전략 + tool subset 정의)
- query cache (Plan 캐싱으로 지터 완화)
- SQL validator (실행 가능 여부 검증)

### rag-service (:8002)

Skill-Plan-Execute 패턴으로 처리.

```
POST /execute
     <- { "skill": str, "input": str, "index": str, "config": {} }
     -> { "results": [...], "plan": {}, "latency_ms": int }

GET  /skills  ->  등록된 skill 목록
```

rag-service 내부 컴포넌트:
- embedding (model_registry에서 모델 선택)
- retrieval (vector search)
- reranking
- skill registry (semantic_search / hybrid_search / multi_hop_retrieval 등)

### pipeline-worker (async)

플랫폼 상태 테이블을 polling하여 작업을 소비한다. HTTP API 없음.

처리 흐름:
```
platform.pipeline_jobs polling (또는 NOTIFY hint)
  → chunking
  → embedding 호출 (rag-service)
  → vector index 갱신
  → platform.pipeline_runs 상태 업데이트
```

## API 모드

pg_aidb는 두 가지 실행 모드를 모두 제공한다. 자세한 결정 배경은 ADR-006 참조.

**동기 모드** — 개발/분석/저동시성. 커넥션 블로킹 수용.

```sql
SELECT ai.search(query => '질문', pipeline => 'docs');
SELECT ai.generate(query => '질문', pipeline => 'docs');
SELECT ai.embed('텍스트', 'openai');
```

**비동기 모드** — 고동시성 프로덕션. 커넥션 즉시 반환.

```sql
SELECT ai.search_async('질문', 'docs')    → request_id
SELECT ai.generate_async('질문', 'docs')  → request_id

-- 결과 수신: NOTIFY 또는 폴링 (클라이언트가 선택)
SELECT * FROM ai.results WHERE id = $1 AND status = 'done';
```

---

## PostgreSQL 함수 인터페이스

```sql
-- Skill-Plan-Execute 공통 인터페이스
SELECT ai.execute(skill => 'semantic_search', input => '사용자 질문');
SELECT ai.execute(skill => 'nl2sql_analytical', input => '지난달 매출 상위 10개');

-- 단일 추론
SELECT ai.predict('model-name', '{"input": "..."}'::jsonb);
SELECT ai.batch_predict('model-name', ARRAY['{"text":"..."}']::jsonb[]);

-- NL2SQL
SELECT ai.nl2sql('최근 7일간 가장 많이 팔린 상품 10개');
SELECT ai.nl2sql('...', skill => 'nl2sql_analytical', model_name => 'gpt-4o');

-- 문서 ingestion
SELECT ai.ingest(content, 'default-pipeline') FROM documents;
SELECT ai.ingest_url('https://...', 'web-pipeline');

-- 벡터 검색
SELECT * FROM ai.vector_search(
    query_vector => embedding,
    index_name   => 'product-index',
    top_k        => 10
);

-- 파이프라인 상태
SELECT * FROM ai.pipeline_runs WHERE status = 'running';

-- endpoint 등록 및 검증
SELECT ai.register_endpoint('inference-prod', 'inference', 'http://inference:8001');
SELECT ai.validate_endpoint('inference-prod');
```

## 벡터 검색 백엔드 전환

```sql
-- 기본값 (GPU 없는 서버)
SET pg_aidb.vector_backend = 'pgvector';

-- GPU 서버 (pg_cuvs 설치 필요)
SET pg_aidb.vector_backend = 'pg_cuvs';
```

## 배포

### 로컬 / 스테이징 - Docker Compose

```
pg_aidb/deploy/
├── docker-compose.yml        # 전체 스택
└── docker-compose.dev.yml    # 소스 마운트 + hot reload
```

| 서비스 | 포트 |
|--------|------|
| PostgreSQL | 5432 |
| inference | 8001 |
| rag | 8002 |
| nl2sql | 8003 |
| pipeline-worker | - (HTTP 없음) |

### 프로덕션 - Helm (Kubernetes)

```
pg_aidb/deploy/helm/
├── Chart.yaml
├── values.yaml
├── values.prod.yaml
└── templates/
    ├── inference/
    ├── rag/
    ├── nl2sql/
    └── pipeline-worker/
```

## 구현 순서

1. `model_registry` SQL 스키마 + CRUD 함수
2. `inference-service` + ONNX 런타임
3. `rag-service` + Skill-Plan-Execute + embedding/retrieval
4. `nl2sql-service` + schema inspector + skill registry + query cache
5. `pipeline-worker` + chunking + indexing
6. 플랫폼 상태 테이블 + extension projection view
7. Docker Compose 전체 스택
8. Helm Chart

---

# 2. pg_cuvs

## 개요

libcuvs (RAPIDS cuVS)를 PostgreSQL extension으로 직접 내장하는 독립 프로젝트.
GPU 가속 벡터 검색을 PostgreSQL 함수로 노출한다.

pgvector나 일반 ANN으로 부족한 규모 - 고차원 대용량 실시간 검색 - 를 타겟으로 한다.
이 규모에서 Python 레이어 오버헤드와 데이터 복사 비용은 GPU 가속 이점을 상쇄하므로
libcuvs를 in-process C++ extension으로 직접 연결한다.

## 언어

**C++ + CUDA**

- libcuvs 자체가 C++ 네이티브. 헤더, 예제, 문서 전부 C++ 기준
- PostgreSQL 함수 등록부는 `extern "C"` wrapping
- CUDA 커널은 `nvcc`로 컴파일
- PL/Python + pylibcuvs 대비: Python 인터프리터 오버헤드 없음, PostgreSQL 메모리에서 CUDA device memory로 직접 복사, libcuvs 전체 API 접근 가능

## 시스템 구성

```
┌─────────────────────────────────────────────────────────┐
│                    Client Application                    │
└─────────────────────┬───────────────────────────────────┘
                      │ SQL
┌─────────────────────▼───────────────────────────────────┐
│                PostgreSQL + pg_cuvs                      │
│                                                         │
│  cuvs.search()      cuvs.create_index()                 │
│  cuvs.upsert()      cuvs.delete_index()                 │
│                                                         │
│  ┌─────────────────────────────────────────────────┐   │
│  │         C++ + CUDA in-process                    │   │
│  │         libcuvs  /  RAPIDS cuVS                  │   │
│  └─────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────┘
                      │ CUDA
              ┌───────▼───────┐
              │   GPU (VRAM)  │
              │  cuVS index   │
              └───────────────┘
```

## 프로젝트 구조

```
pg_cuvs/
├── src/
│   ├── pg_cuvs.cpp       # PostgreSQL 함수 등록 (extern "C")
│   ├── index.cu          # CUDA 커널 + libcuvs 호출
│   └── index.hpp
├── sql/
│   └── pg_cuvs--1.0.sql  # 함수 정의
├── pg_cuvs.control
└── Makefile              # PGXS + nvcc
```

## PostgreSQL 함수 인터페이스

```sql
SELECT cuvs.create_index(index_name => 'product-index', dimension => 1536, metric => 'cosine');
SELECT cuvs.upsert(index_name => 'product-index', ids => ARRAY[1,2,3], vectors => ARRAY[...]::vector[]);
SELECT * FROM cuvs.search(index_name => 'product-index', query_vector => '[...]'::vector, top_k => 10);
SELECT cuvs.drop_index('product-index');
```

## 빌드

```makefile
MODULES    = pg_cuvs
EXTENSION  = pg_cuvs
DATA       = sql/pg_cuvs--1.0.sql
CXX        = g++
NVCC       = nvcc
CUDA_FLAGS = -arch=sm_80 -O3
PG_CONFIG  = pg_config
PGXS      := $(shell $(PG_CONFIG) --pgxs)
include $(PGXS)
```

## 배포

pg_cuvs는 `.so` 파일 단독 배포. Docker Compose / Helm 불필요.

```
GPU 서버    → pg_aidb + pg_cuvs 설치 / SET pg_aidb.vector_backend = 'pg_cuvs'
일반 서버  → pg_aidb만 설치       / SET pg_aidb.vector_backend = 'pgvector'
```

## 구현 순서

1. `cuvs.create_index` / `cuvs.drop_index`
2. `cuvs.upsert`
3. `cuvs.search`
4. pg_aidb vector_backend 연동

---

# 리포지토리 구조

```
github.com/org/pg_aidb    # AI/RAG extension + MSA 서비스
github.com/org/pg_cuvs    # GPU 벡터 검색 extension
```

별도 리포지토리로 분리한다. 빌드 시스템, CI, 의존성, 릴리즈 주기가 완전히 다르다.
