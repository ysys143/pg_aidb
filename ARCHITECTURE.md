# PostgreSQL AI Extension - Architecture

## 시스템 구성

```
┌─────────────────────────────────────────────────────────┐
│                    Client Application                    │
└─────────────────────┬───────────────────────────────────┘
                      │ SQL
┌─────────────────────▼───────────────────────────────────┐
│                 PostgreSQL + pg_ai                       │
│                                                         │
│  ai.predict()  ai.nl2sql()  ai.ingest_document()        │
│  ai.vector_search()  ai.batch_predict()                 │
│                                                         │
│  ┌─────────────────────────────────────────────────┐   │
│  │              model_registry                      │   │
│  │  ai_models / ai_endpoints / ai_model_versions    │   │
│  └──────────────────┬──────────────────────────────┘   │
│                     │ HTTP                              │
└─────────────────────┼───────────────────────────────────┘
                      │
        ┌─────────────┼─────────────────┐
        │             │                 │
        ▼             ▼                 ▼             ▼
┌───────────┐ ┌───────────┐ ┌────────────┐ ┌────────────┐
│ inference │ │  nl2sql   │ │  document  │ │    cuvs    │
│  :8001    │ │  :8003    │ │  :8002     │ │  :8004     │
│           │ │           │ │            │ │            │
│ ONNX      │ │ schema    │ │ chunking   │ │ RAPIDS     │
│ runtime   │ │ inspector │ │ embedding  │ │ cuVS GPU   │
└───────────┘ └───────────┘ └────────────┘ └────────────┘
```

## 설계 원칙

Extension은 얇게 유지한다. 카탈로그 관리와 요청 라우팅만 담당하고,
무거운 연산(ONNX 추론, GPU 벡터 검색, LLM 호출, 문서 파싱)은 모두 외부 서비스로 분리한다.

이유:
- Extension crash = PostgreSQL crash. 불안정한 연산을 in-process에 두지 않는다.
- 서비스별 독립 스케일링 (GPU 서버, CPU 서버 분리 배치 가능)
- 모델/서비스 교체 시 DB 재시작 불필요

---

## model_registry - 단일 진실 원천

모든 모델과 서비스 endpoint를 여기서 관리한다.
어떤 기능이든 model_registry를 통해 모델을 조회하고 라우팅한다.

```sql
-- 모델 카탈로그
CREATE TABLE ai.models (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name        text UNIQUE NOT NULL,
    version     text NOT NULL DEFAULT '1.0',
    model_type  text NOT NULL,        -- 'onnx' | 'llm' | 'embedding' | 'reranker'
    provider    text NOT NULL,        -- 'openai' | 'anthropic' | 'vllm' | 'ollama' | 'local'
    endpoint_id uuid REFERENCES ai.endpoints(id),
    config      jsonb NOT NULL DEFAULT '{}',
    is_default  boolean NOT NULL DEFAULT false,
    created_at  timestamptz NOT NULL DEFAULT now()
);

-- 서비스 endpoint 등록
CREATE TABLE ai.endpoints (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name        text UNIQUE NOT NULL,
    service     text NOT NULL,        -- 'inference' | 'nl2sql' | 'document' | 'cuvs'
    base_url    text NOT NULL,
    api_key_env text,                 -- 환경변수 이름 (값 직접 저장 금지)
    health_url  text GENERATED ALWAYS AS (base_url || '/health') STORED,
    is_active   boolean NOT NULL DEFAULT true,
    last_checked_at timestamptz
);

-- 버전 이력
CREATE TABLE ai.model_versions (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    model_id    uuid NOT NULL REFERENCES ai.models(id),
    version     text NOT NULL,
    artifact_uri text,               -- s3://, gs://, /local/path
    metadata    jsonb NOT NULL DEFAULT '{}',
    promoted_at timestamptz,
    UNIQUE (model_id, version)
);
```

---

## 서비스 API 규약

모든 서비스는 아래 공통 인터페이스를 구현한다.

### 공통

```
GET  /health
     -> { "status": "ok", "version": "x.y.z" }

GET  /models
     -> { "models": [...] }
```

### inference-service

```
POST /predict
     <- { "model_name": str, "input": any, "config": {} }
     -> { "output": any, "model": str, "latency_ms": int }

POST /predict/batch
     <- { "model_name": str, "inputs": [any], "config": {} }
     -> { "outputs": [any], "model": str, "latency_ms": int }
```

### nl2sql-service

```
POST /nl2sql
     <- { "query": str, "schema": str, "model_name": str, "dialect": "postgresql" }
     -> { "sql": str, "explanation": str, "confidence": float }
```

### document-service

```
POST /ingest
     <- { "content": str, "pipeline": str, "metadata": {} }
     -> { "doc_id": uuid, "chunks": int, "status": "queued" | "done" }

POST /ingest/url
     <- { "url": str, "pipeline": str }
     -> { "doc_id": uuid, "status": "queued" }
```

### cuvs-service

```
POST /search
     <- { "index": str, "vector": [float], "top_k": int, "filter": {} }
     -> { "results": [{ "id": any, "score": float, "metadata": {} }] }

POST /index/create
     <- { "index_name": str, "dimension": int, "metric": "cosine" | "l2" | "ip" }
     -> { "index_name": str, "status": "created" }

POST /index/upsert
     <- { "index_name": str, "vectors": [{ "id": any, "vector": [float], "metadata": {} }] }
     -> { "upserted": int }
```

---

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
SELECT ai.ingest_document(content, 'default-pipeline') FROM documents;
SELECT ai.ingest_url('https://...', 'web-pipeline');

-- 벡터 검색
SELECT * FROM ai.vector_search(
    query_vector => embedding,
    index_name   => 'product-index',
    top_k        => 10
);

-- endpoint 등록 및 검증
SELECT ai.register_endpoint('inference-prod', 'inference', 'http://inference:8001');
SELECT ai.validate_endpoint('inference-prod');  -- /health 호출 후 결과 반환
```

---

## 배포

### 로컬 / 스테이징 - Docker Compose

```
deploy/
└── docker-compose.yml      # 전체 스택 (postgres + 4개 서비스)
    docker-compose.dev.yml  # 소스 마운트 + hot reload override
```

```bash
# 로컬 전체 스택 실행
docker compose up

# 개발 모드 (소스 hot reload)
docker compose -f docker-compose.yml -f docker-compose.dev.yml up
```

서비스 포트:
| 서비스 | 포트 |
|--------|------|
| PostgreSQL | 5432 |
| inference | 8001 |
| document | 8002 |
| nl2sql | 8003 |
| cuvs | 8004 |

### 프로덕션 - Helm (Kubernetes)

```
deploy/helm/
├── Chart.yaml
├── values.yaml           # 기본값
├── values.prod.yaml      # 프로덕션 override
└── templates/
    ├── inference/        # Deployment + Service
    ├── document/
    ├── nl2sql/
    └── cuvs/             # nvidia.com/gpu resource + nodeSelector
```

```bash
# 설치
helm install pg-ai ./deploy/helm -f values.prod.yaml

# 업그레이드
helm upgrade pg-ai ./deploy/helm -f values.prod.yaml

# cuvs만 스케일
kubectl scale deployment cuvs -n pg-ai --replicas=3
```

cuvs는 `nvidia.com/gpu: 1` resource request와 GPU nodeSelector를 별도 적용한다.

---

## 구현 순서

1. `model_registry` SQL 스키마 + CRUD 함수
2. `inference-service` + ONNX 런타임
3. `nl2sql-service` + schema inspector + OpenAI-compatible LLM 클라이언트
4. `document-service` + chunking + embedding pipeline
5. `cuvs-service` + RAPIDS cuVS 인덱스
6. Docker Compose 전체 스택
7. Helm Chart
