-- rag_hybrid.sql: TDD scaffold for A7 hybrid search (BM25 + dense + RRF).
--
-- Phase A (red): asserts schema + function presence that doesn't exist yet.
-- Phase B (green): same assertions pass after schema/function landing.
--
-- Usage: docker compose exec pg psql -U postgres -d aidb -e \
--          -f /workspace/extension/test/sql/rag_hybrid.sql

\set ON_ERROR_STOP on

CREATE EXTENSION IF NOT EXISTS pg_aidb;

DO $$
BEGIN
    INSERT INTO ai.endpoints(name, service, base_url)
    VALUES ('rag-svc', 'rag', 'http://rag:8002')
    ON CONFLICT (name) DO UPDATE SET base_url = EXCLUDED.base_url;

    INSERT INTO ai.models(name, model_type, provider, endpoint_id)
    SELECT 'default', 'embedding', 'openai', id FROM ai.endpoints WHERE name = 'rag-svc'
    ON CONFLICT (name) DO NOTHING;

    PERFORM ai.create_pipeline(
        'e2e-hybrid',
        'e2e-hybrid-col',
        'default',
        'default-llm',
        '{}'
    );
END $$;

-- Korean content so BM25 (textsearch_ko MeCab) has something to tokenize.
SELECT pg_typeof(
    ai.ingest(
        'e2e-hybrid-doc.txt',
        'PostgreSQL은 오픈소스 관계형 데이터베이스 시스템이다. ACID 보장과 복잡한 쿼리, 외래 키, 확장 기능을 지원한다. pgvector 확장으로 벡터 유사도 검색을 추가할 수 있다.',
        'e2e-hybrid'
    )
) = 'uuid'::regtype AS ingest_uuid_ok;

DO $$
DECLARE
    deadline timestamptz := now() + interval '60 seconds';
    stat     text;
BEGIN
    LOOP
        SELECT status INTO stat FROM ai.results
        WHERE data->>'op' = 'ingest' AND data->>'source' = 'e2e-hybrid-doc.txt'
        ORDER BY created_at DESC LIMIT 1;
        EXIT WHEN stat IN ('done', 'error') OR now() > deadline;
        PERFORM pg_sleep(1);
    END LOOP;
    IF stat != 'done' THEN
        RAISE EXCEPTION 'Ingestion did not complete (status=%). Check pipeline-worker logs.', stat;
    END IF;
END $$;

SELECT 'ingestion complete' AS status;

-- ASSERTION 1: content_tsv column exists on ai.chunks
SELECT EXISTS(
    SELECT 1 FROM information_schema.columns
    WHERE table_schema = 'ai' AND table_name = 'chunks' AND column_name = 'content_tsv'
) AS content_tsv_column_exists;

-- ASSERTION 2: ai.search_hybrid function exists
SELECT EXISTS(
    SELECT 1 FROM pg_proc p
    JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE n.nspname = 'ai' AND p.proname = 'search_hybrid'
) AS search_hybrid_function_exists;

-- ASSERTION 3: hybrid search returns at least one row
SELECT COUNT(*) > 0 AS hybrid_returns_rows
FROM ai.search_hybrid('PostgreSQL', 'e2e-hybrid', 5);

-- Cleanup
DELETE FROM ai.documents WHERE collection = 'e2e-hybrid-col';
DELETE FROM ai.pipelines WHERE name = 'e2e-hybrid';
DELETE FROM ai.results WHERE data->>'op' = 'ingest' AND data->>'source' = 'e2e-hybrid-doc.txt';

SELECT 'hybrid TDD test complete — all assertions passed' AS result;
