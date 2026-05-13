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
│  ai.predict()       ai.nl2sql()      ai.ingest()        │
│  ai.vector_search() ai.batch_predict()                  │
│                                                         │
│  ┌─────────────────────────────────────────────────┐   │
│  │              model_registry                      │   │
│  │  ai.models / ai.endpoints / ai.model_versions    │   │
│  └──────────────────┬──────────────────────────────┘   │
│                     │ HTTP                              │
└─────────────────────┼───────────────────────────────────┘
                      │
        ┌──────────┬──────────┬──────────┐
        ▼          ▼          ▼          ▼
┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐
│inference │ │  nl2sql  │ │   rag    │ │ pipeline │
│  :8001   │ │  :8003   │ │  :8002   │ │  worker  │
│  Python  │ │  Python  │ │  Python  │ │  Python  │
│  FastAPI │ │  FastAPI │ │  FastAPI │ │          │
│  onnxrt  │ │  schema  │ │ embed /  │ │ chunking │
│          │ │  inspect │ │ retrieve │ │ indexing │
└──────────┘ └──────────┘ └──────────┘ └──────────┘

  벡터 검색 백엔드 (pg_aidb.vector_backend로 선택)
  ┌──────────────────────┬──────────────────────┐
  │ pgvector (기본값)     │ pg_cuvs (GPU 서버)   │
  │ SET vector_backend=  │ SET vector_backend=  │
  │ 'pgvector'           │ 'pg_cuvs'            │
  └──────────────────────┴──────────────────────┘
```

## 설계 원칙

Extension은 얇게 유지한다. 카탈로그 관리와 요청 라우팅만 담당하고,
무거운 연산(ONNX 추론, LLM 호출, 문서 파싱)은 모두 외부 서비스로 분리한다.

- Extension crash = PostgreSQL crash. 불안정한 연산을 in-process에 두지 않는다.
- 서비스별 독립 스케일링 (GPU 서버, CPU 서버 분리 배치 가능)
- 모델/서비스 교체 시 DB 재시작 불필요

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
    service     text NOT NULL,        -- 'inference' | 'nl2sql' | 'document'
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

모든 LLM은 OpenAI-compatible API로 추상화한다.
model_registry의 base_url + api_key_env로 어떤 provider든 교체 가능.

```
POST /nl2sql
     <- { "query": str, "schema": str, "model_name": str, "dialect": "postgresql" }
     -> { "sql": str, "explanation": str, "confidence": float }
```

### document-service (:8002)

```
POST /ingest
     <- { "content": str, "pipeline": str, "metadata": {} }
     -> { "doc_id": uuid, "chunks": int, "status": "queued" | "done" }

POST /ingest/url
     <- { "url": str, "pipeline": str }
     -> { "doc_id": uuid, "status": "queued" }
```

## PostgreSQL 함수 인터페이스

```sql
-- 단일 추론
SELECT ai.predict('model-name', '{"input": "..."}'::jsonb);

-- 배치 추론
SELECT ai.batch_predict('model-name', ARRAY['{"text":"..."}', ...]::jsonb[]);

-- NL2SQL (model_name 생략 시 default 모델 사용)
SELECT ai.nl2sql('최근 7일간 가장 많이 팔린 상품 10개');
SELECT ai.nl2sql('...', model_name => 'gpt-4o');

-- 문서 ingestion
SELECT ai.ingest(content, 'default-pipeline') FROM documents;
SELECT ai.ingest_url('https://...', 'web-pipeline');

-- 벡터 검색 (backend는 GUC로 선택)
SELECT * FROM ai.vector_search(
    query_vector => embedding,
    index_name   => 'product-index',
    top_k        => 10
);

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

pg_aidb는 GUC 값을 읽어 라우팅만 한다.
pg_cuvs가 설치되지 않은 상태에서 `pg_cuvs`로 설정하면 에러를 반환한다.

## 배포

### 로컬 / 스테이징 - Docker Compose

```
pg_aidb/deploy/
├── docker-compose.yml        # 전체 스택 (postgres + 3개 서비스)
└── docker-compose.dev.yml    # 소스 마운트 + hot reload override
```

서비스 포트:

| 서비스 | 포트 |
|--------|------|
| PostgreSQL | 5432 |
| inference | 8001 |
| document | 8002 |
| nl2sql | 8003 |

### 프로덕션 - Helm (Kubernetes)

```
pg_aidb/deploy/helm/
├── Chart.yaml
├── values.yaml
├── values.prod.yaml
└── templates/
    ├── inference/
    ├── document/
    └── nl2sql/
```

## 구현 순서

1. `model_registry` SQL 스키마 + CRUD 함수
2. `inference-service` + ONNX 런타임
3. `nl2sql-service` + schema inspector + OpenAI-compatible 클라이언트
4. `document-service` + chunking + embedding pipeline
5. Docker Compose 전체 스택
6. Helm Chart

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
-- 인덱스 생성
SELECT cuvs.create_index(
    index_name => 'product-index',
    dimension  => 1536,
    metric     => 'cosine'    -- 'cosine' | 'l2' | 'ip'
);

-- 벡터 upsert
SELECT cuvs.upsert(
    index_name => 'product-index',
    ids        => ARRAY[1, 2, 3],
    vectors    => ARRAY[...]::vector[]
);

-- 검색
SELECT * FROM cuvs.search(
    index_name   => 'product-index',
    query_vector => '[0.1, 0.2, ...]'::vector,
    top_k        => 10
);

-- 인덱스 삭제
SELECT cuvs.drop_index('product-index');
```

## 빌드

```makefile
# Makefile
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

```bash
make
sudo make install
psql -c "CREATE EXTENSION pg_cuvs;"
```

## 배포

pg_cuvs는 `.so` 파일 단독 배포. Docker Compose / Helm 불필요.
GPU 노드에만 설치하고 pg_aidb의 `vector_backend = 'pg_cuvs'`로 연결한다.

```
GPU 서버    → pg_aidb + pg_cuvs 설치
              SET pg_aidb.vector_backend = 'pg_cuvs';

일반 서버  → pg_aidb만 설치
              SET pg_aidb.vector_backend = 'pgvector';
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
