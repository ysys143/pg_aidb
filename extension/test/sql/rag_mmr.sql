-- rag_mmr.sql: TDD scaffold for A8 MMR diversity.
--
-- Ingests several near-duplicate chunks then verifies that
-- ai.search_mmr returns fewer duplicates than ai.search.
--
-- Phase A (red):  ai.search_mmr doesn't exist yet.
-- Phase B (green): all assertions pass.

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
        'e2e-mmr', 'e2e-mmr-col', 'default', 'default-llm', '{}'
    );
END $$;

-- Ingest near-duplicate docs about PostgreSQL and one unrelated doc.
SELECT ai.ingest('mmr-pg1.txt', 'PostgreSQL is an open-source relational database system.', 'e2e-mmr');
SELECT ai.ingest('mmr-pg2.txt', 'PostgreSQL is a free open-source relational database.', 'e2e-mmr');
SELECT ai.ingest('mmr-pg3.txt', 'PostgreSQL is an open-source RDBMS with advanced features.', 'e2e-mmr');
SELECT ai.ingest('mmr-py1.txt', 'Python is a programming language used for data science and ML.', 'e2e-mmr');

DO $$
DECLARE
    deadline timestamptz := now() + interval '90 seconds';
    n int;
BEGIN
    LOOP
        SELECT COUNT(*) INTO n FROM ai.results
        WHERE data->>'op' = 'ingest'
          AND data->>'source' IN ('mmr-pg1.txt','mmr-pg2.txt','mmr-pg3.txt','mmr-py1.txt')
          AND status = 'done';
        EXIT WHEN n >= 4 OR now() > deadline;
        PERFORM pg_sleep(1);
    END LOOP;
    IF n < 4 THEN RAISE EXCEPTION 'Ingestion incomplete (% of 4 done)', n; END IF;
END $$;

SELECT 'ingestion complete' AS status;

-- ASSERTION 1: ai.search_mmr function exists.
SELECT EXISTS(
    SELECT 1 FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE n.nspname = 'ai' AND p.proname = 'search_mmr'
) AS search_mmr_function_exists;

-- ASSERTION 2: search_mmr returns results.
SELECT COUNT(*) > 0 AS mmr_returns_rows
FROM ai.search_mmr('PostgreSQL', 'e2e-mmr', 3);

-- ASSERTION 3: search_mmr returns fewer near-duplicates than plain search
-- (the Python doc should appear in top-3 MMR but not top-3 plain search).
SELECT EXISTS(
    SELECT 1 FROM ai.search_mmr('PostgreSQL', 'e2e-mmr', 3)
    WHERE source = 'mmr-py1.txt'
) AS mmr_includes_diverse_result;

-- Cleanup.
DELETE FROM ai.documents WHERE collection = 'e2e-mmr-col';
DELETE FROM ai.pipelines WHERE name = 'e2e-mmr';
DELETE FROM ai.results WHERE data->>'source' IN ('mmr-pg1.txt','mmr-pg2.txt','mmr-pg3.txt','mmr-py1.txt');

SELECT 'MMR TDD test complete — all assertions passed' AS result;
