-- rag_async.sql: validates ai.search_async() and ai.ask_async() end-to-end.
--
-- IMPORTANT: PL/pgSQL DO blocks run in ONE transaction by default — pipeline-worker
-- (separate session) cannot see ai._outbox / ai.results rows until COMMIT, and NOTIFY
-- is only delivered on commit. So we use explicit COMMIT after each async kickoff
-- before polling. PG11+ allows COMMIT inside top-level DO blocks.
\set ON_ERROR_STOP on

CREATE EXTENSION IF NOT EXISTS pg_aidb;

DO $$ BEGIN
    INSERT INTO ai.endpoints(name, service, base_url)
    VALUES ('rag-svc', 'rag', 'http://rag:8002')
    ON CONFLICT (name) DO UPDATE SET base_url = EXCLUDED.base_url;
    INSERT INTO ai.models(name, model_type, provider, endpoint_id)
    SELECT 'default', 'embedding', 'openai', id FROM ai.endpoints WHERE name='rag-svc'
    ON CONFLICT (name) DO NOTHING;
    PERFORM ai.create_pipeline('async-test', 'async-test-col', 'default', 'default-llm', '{}');
END $$;

-- Seed: synchronous ingest so search/ask have data
DO $$
DECLARE
    req uuid;
    stat text;
    deadline timestamptz;
BEGIN
    req := ai.ingest('/data/sample.pdf', '', 'async-test');
    COMMIT;  -- pipeline-worker sees the row + NOTIFY only after commit
    deadline := now() + interval '120 seconds';
    LOOP
        SELECT status INTO stat FROM ai.results WHERE request_id = req;
        EXIT WHEN stat IN ('done','error') OR now() > deadline;
        PERFORM pg_sleep(2);
    END LOOP;
    IF stat != 'done' THEN RAISE EXCEPTION 'seed ingest failed (%)', stat; END IF;
END $$;

SELECT 'seed ingest complete' AS status;

-- search_async
DO $$
DECLARE
    req uuid;
    stat text;
    n_results int;
    deadline timestamptz;
BEGIN
    req := ai.search_async('What is PostgreSQL?', 'async-test', 3);
    COMMIT;
    deadline := now() + interval '60 seconds';
    LOOP
        SELECT status INTO stat FROM ai.results WHERE request_id = req;
        EXIT WHEN stat IN ('done','error') OR now() > deadline;
        PERFORM pg_sleep(1);
    END LOOP;
    IF stat != 'done' THEN RAISE EXCEPTION 'search_async did not complete (%)', stat; END IF;
    SELECT jsonb_array_length(data->'results') INTO n_results
    FROM ai.results WHERE request_id = req;
    IF n_results IS NULL OR n_results = 0 THEN
        RAISE EXCEPTION 'search_async returned no results';
    END IF;
    RAISE NOTICE 'search_async OK: % results', n_results;
END $$;

-- ask_async
DO $$
DECLARE
    req uuid;
    stat text;
    answer text;
    deadline timestamptz;
BEGIN
    req := ai.ask_async('What is PostgreSQL?', 'async-test');
    COMMIT;
    deadline := now() + interval '120 seconds';
    LOOP
        SELECT status INTO stat FROM ai.results WHERE request_id = req;
        EXIT WHEN stat IN ('done','error') OR now() > deadline;
        PERFORM pg_sleep(2);
    END LOOP;
    IF stat != 'done' THEN RAISE EXCEPTION 'ask_async did not complete (%)', stat; END IF;
    SELECT data->>'answer' INTO answer FROM ai.results WHERE request_id = req;
    IF answer IS NULL OR LENGTH(answer) = 0 THEN
        RAISE EXCEPTION 'ask_async returned empty answer';
    END IF;
    RAISE NOTICE 'ask_async OK: answer length=%', LENGTH(answer);
END $$;

-- Cleanup
DELETE FROM ai.documents WHERE collection = 'async-test-col';
DELETE FROM ai.pipelines WHERE name = 'async-test';
DELETE FROM ai.results WHERE data->>'pipeline' = 'async-test'
                          OR data->>'source' = '/data/sample.pdf';

SELECT 'async E2E complete (ingest + search_async + ask_async)' AS result;
