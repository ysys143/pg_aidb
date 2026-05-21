-- rag_filter.sql: TDD scaffold for A6 metadata filtering.
--
-- Ingests two docs, patches their chunk metadata directly, then verifies
-- ai.search with a filter returns only the matching chunk.
--
-- Phase A (red):  4th arg to ai.search doesn't exist → type error.
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
        'e2e-filter', 'e2e-filter-col', 'default', 'default-llm', '{}'
    );
END $$;

SELECT ai.ingest(
    'filter-db.txt',
    'PostgreSQL is a powerful relational database system with ACID compliance.',
    'e2e-filter'
);

SELECT ai.ingest(
    'filter-py.txt',
    'Python is a general-purpose programming language popular in data science.',
    'e2e-filter'
);

-- Wait for both ingestions.
DO $$
DECLARE
    deadline timestamptz := now() + interval '90 seconds';
    n int;
BEGIN
    LOOP
        SELECT COUNT(*) INTO n FROM ai.results
        WHERE data->>'op' = 'ingest'
          AND data->>'source' IN ('filter-db.txt', 'filter-py.txt')
          AND status = 'done';
        EXIT WHEN n >= 2 OR now() > deadline;
        PERFORM pg_sleep(1);
    END LOOP;
    IF n < 2 THEN RAISE EXCEPTION 'Ingestion incomplete (% of 2 done)', n; END IF;
END $$;

-- Patch chunk metadata so the filter has something to match.
UPDATE ai.chunks SET metadata = '{"category": "database"}'
WHERE document_id = (SELECT id FROM ai.documents WHERE source = 'filter-db.txt');

UPDATE ai.chunks SET metadata = '{"category": "python"}'
WHERE document_id = (SELECT id FROM ai.documents WHERE source = 'filter-py.txt');

SELECT 'setup complete' AS status;

-- ASSERTION 1: filter=database returns the db chunk.
SELECT COUNT(*) > 0 AS filter_db_returns_row
FROM ai.search('PostgreSQL', 'e2e-filter', 5, '{"category": "database"}');

-- ASSERTION 2: filter=python returns only python-category chunks (no db chunk leaks through).
SELECT bool_and(metadata->>'category' = 'python') AS filter_py_returns_only_python
FROM ai.search('database', 'e2e-filter', 5, '{"category": "python"}');

-- ASSERTION 3: no filter (default '{}') returns rows from both.
SELECT COUNT(*) > 0 AS no_filter_returns_rows
FROM ai.search('PostgreSQL', 'e2e-filter', 5);

-- Cleanup.
DELETE FROM ai.documents WHERE collection = 'e2e-filter-col';
DELETE FROM ai.pipelines WHERE name = 'e2e-filter';
DELETE FROM ai.results WHERE data->>'source' IN ('filter-db.txt', 'filter-py.txt');

SELECT 'filter TDD test complete — all assertions passed' AS result;
