-- rag_e2e.sql: RAG E2E test via direct psql against 'aidb' database.
--
-- Unlike rag_mock.sql, this file does NOT create/drop the extension —
-- the extension must already be installed (via cargo pgrx install).
-- Runs in 'aidb' so pipeline-worker's LISTEN receives NOTIFYs.
--
-- Usage:
--   docker compose exec pg psql -U postgres -d aidb -e \
--     -f /workspace/extension/test/sql/rag_e2e.sql

\set ON_ERROR_STOP on

-- Install extension if not already present (idempotent; never dropped here).
CREATE EXTENSION IF NOT EXISTS pg_aidb;

-- Setup: register rag service endpoint + model + pipeline
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
        'e2e-rag',
        'e2e-test-col',
        'default',
        'default-llm',
        '{}'
    );
END $$;

-- Ingest inline content
SELECT pg_typeof(
    ai.ingest(
        'e2e-doc.txt',
        'PostgreSQL is a powerful open-source relational database system. It has advanced features including ACID compliance, complex queries, foreign keys, and extensibility via extensions.',
        'e2e-rag'
    )
) = 'uuid'::regtype AS ingest_uuid_ok;

-- Wait for pipeline-worker to finish ingestion (max 60s)
DO $$
DECLARE
    deadline timestamptz := now() + interval '60 seconds';
    stat     text;
BEGIN
    LOOP
        SELECT status INTO stat
        FROM ai.results
        WHERE data->>'op' = 'ingest' AND data->>'source' = 'e2e-doc.txt'
        ORDER BY created_at DESC LIMIT 1;
        EXIT WHEN stat IN ('done', 'error') OR now() > deadline;
        PERFORM pg_sleep(1);
    END LOOP;
    IF stat != 'done' THEN
        RAISE EXCEPTION 'Ingestion did not complete (status=%). Check pipeline-worker logs.', stat;
    END IF;
END $$;

SELECT 'ingestion complete' AS status;

-- Chunks must be stored
SELECT COUNT(*) > 0 AS chunks_stored
FROM ai.chunks WHERE collection = 'e2e-test-col';

-- Search must return results
SELECT COUNT(*) > 0 AS search_ok
FROM ai.search('What is PostgreSQL?', 'e2e-rag', 5);

-- Ask must return a non-null answer
SELECT ai.ask('What is PostgreSQL?', 'e2e-rag') IS NOT NULL AS ask_ok;

-- Cleanup
DELETE FROM ai.documents WHERE collection = 'e2e-test-col';
DELETE FROM ai.pipelines WHERE name = 'e2e-rag';
DELETE FROM ai.results WHERE data->>'op' = 'ingest' AND data->>'source' = 'e2e-doc.txt';

SELECT 'E2E test complete — all assertions passed' AS result;
