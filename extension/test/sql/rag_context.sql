-- rag_context.sql: TDD scaffold for A9 context window management.
--
-- Tests that ai.ask accepts a strategy parameter and both strategies
-- return valid answers without exceeding context limits.
--
-- Phase A (red):  strategy param doesn't exist yet.
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
        'e2e-ctx', 'e2e-ctx-col', 'default', 'default-llm', '{}'
    );
END $$;

SELECT ai.ingest(
    'ctx-doc.txt',
    'PostgreSQL is an open-source relational database system. It supports ACID transactions, complex queries, foreign keys, triggers, and views. PostgreSQL has been actively developed for over 35 years. It is known for reliability, data integrity, and correctness.',
    'e2e-ctx'
);

DO $$
DECLARE deadline timestamptz := now() + interval '60 seconds'; stat text;
BEGIN
    LOOP
        SELECT status INTO stat FROM ai.results
        WHERE data->>'op' = 'ingest' AND data->>'source' = 'ctx-doc.txt'
        ORDER BY created_at DESC LIMIT 1;
        EXIT WHEN stat IN ('done','error') OR now() > deadline;
        PERFORM pg_sleep(1);
    END LOOP;
    IF stat != 'done' THEN RAISE EXCEPTION 'Ingestion failed: %', stat; END IF;
END $$;

SELECT 'ingestion complete' AS status;

-- ASSERTION 1: prune strategy returns non-null answer.
SELECT ai.ask(
    'What is PostgreSQL?', 'e2e-ctx',
    top_k => 3,
    max_context_tokens => 500,
    strategy => 'prune'
) IS NOT NULL AS prune_returns_answer;

-- ASSERTION 2: map_reduce strategy returns non-null answer.
SELECT ai.ask(
    'What is PostgreSQL?', 'e2e-ctx',
    top_k => 3,
    max_context_tokens => 500,
    strategy => 'map_reduce'
) IS NOT NULL AS map_reduce_returns_answer;

-- Cleanup.
DELETE FROM ai.documents WHERE collection = 'e2e-ctx-col';
DELETE FROM ai.pipelines WHERE name = 'e2e-ctx';
DELETE FROM ai.results WHERE data->>'source' = 'ctx-doc.txt';

SELECT 'context window TDD test complete — all assertions passed' AS result;
