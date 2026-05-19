-- rag_parse.sql: validates opendataloader PDF parsing path end-to-end.
--
-- Mounts: pipeline-worker reads /data/sample.pdf (./tests/fixtures/sample.pdf on host).
-- Usage:
--   make run-rag-parse-mock    -- offline, mock OpenAI
--   make run-rag-parse-real    -- real OpenAI, requires OPENAI_API_KEY in .env
\set ON_ERROR_STOP on

CREATE EXTENSION IF NOT EXISTS pg_aidb;

-- Register rag endpoint + default model + parse-test pipeline
DO $$
BEGIN
    INSERT INTO ai.endpoints(name, service, base_url)
    VALUES ('rag-svc', 'rag', 'http://rag:8002')
    ON CONFLICT (name) DO UPDATE SET base_url = EXCLUDED.base_url;

    INSERT INTO ai.models(name, model_type, provider, endpoint_id)
    SELECT 'default', 'embedding', 'openai', id
    FROM ai.endpoints WHERE name = 'rag-svc'
    ON CONFLICT (name) DO NOTHING;

    PERFORM ai.create_pipeline(
        'parse-test', 'parse-test-col', 'default', 'default-llm', '{}'
    );
END $$;

-- Empty content → pipeline-worker parses /data/sample.pdf via opendataloader
SELECT pg_typeof(
    ai.ingest('/data/sample.pdf', '', 'parse-test')
) = 'uuid'::regtype AS ingest_uuid_ok;

-- Wait for ingestion (max 90s; PDF parsing + JVM spawn is slower than inline)
DO $$
DECLARE
    deadline timestamptz := now() + interval '90 seconds';
    stat     text;
BEGIN
    LOOP
        SELECT status INTO stat FROM ai.results
        WHERE data->>'op' = 'ingest' AND data->>'source' = '/data/sample.pdf'
        ORDER BY created_at DESC LIMIT 1;
        EXIT WHEN stat IN ('done', 'error') OR now() > deadline;
        PERFORM pg_sleep(2);
    END LOOP;
    IF stat != 'done' THEN
        RAISE EXCEPTION 'PDF ingestion did not complete (status=%). Check pipeline-worker logs.', stat;
    END IF;
END $$;

-- Chunks must exist
SELECT COUNT(*) > 0 AS chunks_stored
FROM ai.chunks WHERE collection = 'parse-test-col';

-- The PDF body literally contains "PostgreSQL" — opendataloader must have extracted it
SELECT bool_or(content LIKE '%PostgreSQL%') AS pdf_content_extracted
FROM ai.chunks WHERE collection = 'parse-test-col';

-- Retrieval must work over the parsed chunks
SELECT COUNT(*) > 0 AS search_ok
FROM ai.search('What is PostgreSQL?', 'parse-test', 5);

-- Cleanup (cascade from documents removes chunks)
DELETE FROM ai.documents WHERE collection = 'parse-test-col';
DELETE FROM ai.pipelines WHERE name = 'parse-test';
DELETE FROM ai.results WHERE data->>'op' = 'ingest' AND data->>'source' = '/data/sample.pdf';

SELECT 'PDF parse E2E complete' AS result;
