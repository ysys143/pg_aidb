-- Asserts seed data round-tripped via pg_dump/restore (C6).
-- Run against the RESTORED database.
\set ON_ERROR_STOP on

DO $$
DECLARE
    n_endpoints int; n_models int; n_pipelines int; n_results int;
BEGIN
    SELECT count(*) INTO n_endpoints FROM ai.endpoints WHERE name = 'dump-test-ep';
    SELECT count(*) INTO n_models    FROM ai.models    WHERE name = 'dump-test-model';
    SELECT count(*) INTO n_pipelines FROM ai.pipelines WHERE name = 'dump-test-pl';
    SELECT count(*) INTO n_results   FROM ai.results   WHERE data->>'op' = 'dump-test';
    IF n_endpoints = 0 THEN RAISE EXCEPTION 'ai.endpoints not restored'; END IF;
    IF n_models    = 0 THEN RAISE EXCEPTION 'ai.models not restored';    END IF;
    IF n_pipelines = 0 THEN RAISE EXCEPTION 'ai.pipelines not restored'; END IF;
    IF n_results   = 0 THEN RAISE EXCEPTION 'ai.results not restored';   END IF;
    RAISE NOTICE 'pg_dump round-trip OK: endpoints=% models=% pipelines=% results=%',
                  n_endpoints, n_models, n_pipelines, n_results;
END $$;
