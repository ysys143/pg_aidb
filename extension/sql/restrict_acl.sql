-- Opt-in restrictive ACL for pg_aidb.
-- Apply when the extension owner (postgres) is different from the application role.
--
-- Usage:
--   psql -d aidb -f sql/restrict_acl.sql
--   psql -d aidb -c "GRANT EXECUTE ON FUNCTION ai.search(text, text, integer) TO my_app_role;"
--
-- This file does NOT grant anything to app roles — it only revokes PUBLIC.
-- Grant explicitly per role per function, following least privilege.

\set ON_ERROR_STOP on

-- 1. Revoke PUBLIC EXECUTE on all ai.* functions
REVOKE EXECUTE ON FUNCTION ai.embed_raw(text, text)                           FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION ai.embed_async(text, text)                         FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION ai.create_pipeline(text, text, text, text, jsonb)  FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION ai.ingest(text, text, text)                        FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION ai.search(text, text, integer)                     FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION ai.ask(text, text)                                 FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION ai.search_async(text, text, integer)               FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION ai.ask_async(text, text)                           FROM PUBLIC;

-- 2. Revoke ALL on tables from PUBLIC (defaults already deny, this is belt + suspenders)
REVOKE ALL ON ai.endpoints, ai.models, ai.pipelines, ai.results, ai._outbox FROM PUBLIC;

-- 3. Use platform_ai.*_v1 views for read access (ADR-004).
--    These are the stable contract surface; api_key_env is already excluded from endpoints_v1.

-- Sample grants (uncomment and adapt to your roles):
--   CREATE ROLE app_user;
--   GRANT EXECUTE ON FUNCTION ai.search(text, text, integer) TO app_user;
--   GRANT EXECUTE ON FUNCTION ai.ask(text, text)             TO app_user;
--   GRANT EXECUTE ON FUNCTION ai.search_async(text, text, integer) TO app_user;
--   GRANT EXECUTE ON FUNCTION ai.ask_async(text, text)       TO app_user;
--   GRANT USAGE ON SCHEMA platform_ai TO app_user;
--   GRANT SELECT ON ALL TABLES IN SCHEMA platform_ai TO app_user;
