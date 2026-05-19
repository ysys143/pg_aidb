# pg_aidb 수동 테스트 플레이북

자동 테스트 외에 직접 손으로 인제스트하고 질의를 던지며 동작을 확인하는 시나리오. 5분이면 처음부터 끝까지 한 사이클이 돕니다.

## 사전 조건

```bash
# 1. .env에 키
cp .env.example .env
# 편집: OPENAI_API_KEY=sk-proj-...  /  LLM_MODEL=gpt-5.4-mini
```

mock 모드로 돌리고 싶으면 키 불필요. 아래 모든 명령에서 `make ... | sed s/real/mock/`로 바꾸거나, 명시적으로 mock 변형 (있는 경우)을 사용.

---

## 1. 스택 기동 + extension 설치

한 번에 (real API):
```bash
cd extension && make run-rag-real
```

이 명령은 자동으로:
1. pg + builder 컨테이너 시작
2. 기존 extension drop → cargo pgrx install → CREATE EXTENSION
3. pipeline-worker + rag 컨테이너 시작 + health 대기
4. `rag_e2e.sql` 자동 E2E 실행

여기서 멈추고 손으로 탐색하고 싶으면:
```bash
docker compose up -d pg mock builder pipeline-worker rag
docker compose exec builder bash -c "
  psql -h pg -U postgres -d aidb -c 'DROP EXTENSION IF EXISTS pg_aidb CASCADE';
  cargo pgrx install --pg-config /usr/bin/pg_config;
  psql -h pg -U postgres -d aidb -c 'CREATE EXTENSION pg_aidb'
"
docker compose restart pipeline-worker
```

상태 확인:
```bash
docker compose ps
# 5개 컨테이너 모두 (healthy)인지 확인
```

---

## 2. 엔드포인트 + 모델 + 파이프라인 등록

psql 접속:
```bash
docker compose exec pg psql -U postgres -d aidb
```

이후는 psql 안에서:
```sql
-- rag 서비스를 임베딩 + LLM 엔드포인트로 등록 (한 번만)
INSERT INTO ai.endpoints(name, service, base_url)
VALUES ('rag-svc', 'rag', 'http://rag:8002')
ON CONFLICT (name) DO UPDATE SET base_url = EXCLUDED.base_url;

INSERT INTO ai.models(name, model_type, provider, endpoint_id)
SELECT 'default', 'embedding', 'openai', id FROM ai.endpoints WHERE name='rag-svc'
ON CONFLICT (name) DO NOTHING;

-- 본인 도메인용 파이프라인
SELECT ai.create_pipeline(
    'my-docs',           -- 파이프라인 이름
    'my-collection',     -- 데이터를 담을 컬렉션
    'default',           -- embed_model
    'default-llm',       -- llm_model
    '{"chunking":{"method":"semantic","percentile":90}}'::jsonb
);

-- 확인
SELECT * FROM platform_ai.pipelines_v1;
```

---

## 3. 문서 인제스트

### 3a. 인라인 텍스트 (PDF 파싱 없이 빠르게)
```sql
SELECT ai.ingest(
    'README.txt',
    'PostgreSQL is a powerful open-source RDBMS. '
    'It supports ACID transactions, advanced data types, JSON, and full-text search. '
    'The pgvector extension adds AI-friendly vector similarity. '
    'pg_aidb wraps RAG flow as native SQL functions.',
    'my-docs'
);
-- 반환된 uuid를 기억 (또는 아래로 추적)
```

### 3b. PDF 파일 (`./tests/fixtures/sample.pdf` 사용)
```sql
SELECT ai.ingest('/data/sample.pdf', '', 'my-docs');
-- 빈 content → pipeline-worker가 opendataloader로 파싱
```

### 3c. 본인 PDF
```bash
# 호스트에서: 파일을 tests/fixtures/ 에 복사
cp ~/Desktop/my-paper.pdf tests/fixtures/

# psql 안에서:
```
```sql
SELECT ai.ingest('/data/my-paper.pdf', '', 'my-docs');
```

### 진행 상황 모니터링
```sql
SELECT request_id, op, status, source, created_at, finished_at
FROM platform_ai.results_v1
ORDER BY created_at DESC LIMIT 5;

-- 또는 status가 done이 될 때까지 폴링
SELECT status FROM ai.results ORDER BY created_at DESC LIMIT 1;
```

pipeline-worker 로그 (별도 터미널):
```bash
docker compose logs -f pipeline-worker
# JSON 라인 — ingest 진행, chunks 수, usage 보임
```

저장된 청크 확인:
```sql
SELECT chunk_index, length(content) AS chars, substring(content, 1, 60) || '...' AS preview
FROM platform_ai.chunks_v1
WHERE collection = 'my-collection'
ORDER BY chunk_index;
```

---

## 4. 검색 (sync)

청크만 반환:
```sql
SELECT content, similarity, source
FROM ai.search('What is PostgreSQL?', 'my-docs', 3);
```

다른 질의도:
```sql
SELECT content, similarity FROM ai.search('vector similarity search', 'my-docs', 3);
SELECT content, similarity FROM ai.search('ACID transactions', 'my-docs', 3);
```

top_k 조절:
```sql
SELECT count(*) FROM ai.search('PostgreSQL', 'my-docs', 10);
```

---

## 5. 질의 (sync, LLM 답변까지)

```sql
SELECT ai.ask('What is PostgreSQL and why use it?', 'my-docs');
SELECT ai.ask('How does pg_aidb relate to pgvector?', 'my-docs');
```

응답은 단일 텍스트. 길어 보고 싶으면:
```sql
\x on
SELECT ai.ask('Summarize the document in 3 bullet points', 'my-docs');
\x off
```

---

## 6. 비동기 변형

비동기는 호출 즉시 uuid 반환, 백그라운드에서 처리:

```sql
-- 1단계: 비동기 검색 시작
SELECT ai.search_async('What is PostgreSQL?', 'my-docs', 3) AS req_id;
-- 반환된 uuid 기억 (예: 'aaa-bbb-...')
COMMIT;  -- 중요: pipeline-worker가 NOTIFY를 받으려면 commit 필요

-- 2단계: 완료 폴링
SELECT status, finished_at - created_at AS elapsed
FROM ai.results WHERE request_id = 'aaa-bbb-...';

-- 3단계: 결과 조회
SELECT jsonb_pretty(data->'results')
FROM ai.results WHERE request_id = 'aaa-bbb-...';
```

`ai.ask_async()`도 같은 패턴 — `data->>'answer'`로 텍스트 답변 추출.

LISTEN으로 푸시 받기 (선택):
```sql
-- 별도 psql 세션에서
LISTEN aidb_pipeline;
-- 다른 세션에서 ai.search_async() 호출하면 알림 도착
```

---

## 7. 토큰 비용 + 지연 추적

```sql
-- 모든 비동기 호출의 토큰 사용
SELECT op, embed_model, llm_model,
       total_tokens, embed_prompt_tokens, llm_prompt_tokens, llm_completion_tokens,
       duration_sec
FROM platform_ai.usage_v1
ORDER BY created_at DESC LIMIT 10;

-- op별 집계
SELECT op,
       COUNT(*)              AS requests,
       SUM(total_tokens)     AS tokens,
       ROUND(AVG(duration_sec)::numeric, 3) AS avg_sec
FROM platform_ai.usage_v1
GROUP BY op;

-- 파이프라인별 누적
SELECT pipeline, SUM(total_tokens) FROM platform_ai.usage_v1 GROUP BY 1;
```

stdout JSON 로그:
```bash
docker compose logs --since=5m rag | grep '"op"' | head
docker compose logs --since=5m pipeline-worker | grep -E '"ingest"|"search"|"ask"' | head
```

---

## 8. 파이프라인 변형 실험

청킹 메서드 비교 — 같은 문서를 4개 파이프라인으로:
```sql
SELECT ai.create_pipeline('p-sem',  'cmp-sem',  'default','default-llm', '{"chunking":{"method":"semantic","percentile":90}}');
SELECT ai.create_pipeline('p-fix',  'cmp-fix',  'default','default-llm', '{"chunking":{"method":"fixed","chunk_size":500,"overlap":50}}');
SELECT ai.create_pipeline('p-rec',  'cmp-rec',  'default','default-llm', '{"chunking":{"method":"recursive","chunk_size":500}}');
SELECT ai.create_pipeline('p-par',  'cmp-par',  'default','default-llm', '{"chunking":{"method":"paragraph","min_size":100,"max_size":600}}');

SELECT ai.ingest('/data/sample.pdf', '', 'p-sem');
SELECT ai.ingest('/data/sample.pdf', '', 'p-fix');
SELECT ai.ingest('/data/sample.pdf', '', 'p-rec');
SELECT ai.ingest('/data/sample.pdf', '', 'p-par');
-- 모두 완료 대기

SELECT collection,
       COUNT(*) AS n_chunks,
       MIN(length(content)) AS min_len,
       AVG(length(content))::int AS avg_len,
       MAX(length(content)) AS max_len
FROM platform_ai.chunks_v1
WHERE collection LIKE 'cmp-%'
GROUP BY collection ORDER BY collection;
```

---

## 9. 정리

```sql
-- 본인 컬렉션의 모든 데이터
DELETE FROM ai.documents WHERE collection = 'my-collection';
-- 비교 컬렉션 정리
DELETE FROM ai.documents WHERE collection LIKE 'cmp-%';
DELETE FROM ai.pipelines WHERE name IN ('my-docs','p-sem','p-fix','p-rec','p-par');
DELETE FROM ai.results WHERE data->>'pipeline' IN ('my-docs','p-sem','p-fix','p-rec','p-par')
                          OR data->>'source' LIKE '/data/%';
```

전체 초기화:
```bash
\q
docker compose exec pg psql -U postgres -d aidb -c "DROP EXTENSION pg_aidb CASCADE"
docker compose down
```

---

## 문제 해결

| 증상 | 확인 |
|---|---|
| `function ai.search does not exist` | `make run-rag-real` 또는 `DROP EXTENSION ... CASCADE; CREATE EXTENSION pg_aidb` 다시 |
| ingest가 영원히 pending | `docker compose logs pipeline-worker --tail=30` — DB 연결, OpenAI 에러, 청킹 실패 등 |
| `OPENAI_API_KEY not set` | `.env`에 키 있는지, Makefile이 `-include`하는지 확인 |
| async가 안 진행됨 | psql 세션이 트랜잭션 열고 있는지 — `COMMIT`이나 autocommit 모드 사용 |
| 청크가 너무 적음/많음 | 파이프라인 `config.chunking`의 percentile/chunk_size 조정 (8번 섹션 참고) |
| `/data/...` 파일을 못 읽음 | `./tests/fixtures` 마운트 — host에 파일 있는지 확인 |
