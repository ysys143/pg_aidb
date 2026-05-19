-- Seed data for verify-dump-restore (C6).
-- Inserts one row per user-data table so we can verify pg_extension_config_dump
-- actually causes pg_dump to include the rows.
\set ON_ERROR_STOP on

INSERT INTO ai.endpoints(name, service, base_url, api_key_env)
VALUES ('dump-test-ep', 'inference', 'http://x.example', 'MY_KEY')
ON CONFLICT (name) DO UPDATE SET base_url = EXCLUDED.base_url;

INSERT INTO ai.models(name, model_type, provider, endpoint_id)
SELECT 'dump-test-model', 'embedding', 'test', id
FROM ai.endpoints WHERE name = 'dump-test-ep'
ON CONFLICT (name) DO NOTHING;

SELECT ai.create_pipeline('dump-test-pl', 'dump-col', 'dump-test-model', 'default-llm', '{"k":1}');

INSERT INTO ai.results(request_id, status, data)
VALUES (gen_random_uuid(), 'done', '{"op":"dump-test"}'::jsonb);
