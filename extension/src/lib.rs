use pgrx::prelude::*;

::pgrx::pg_module_magic!(name, version);

mod client;
mod registry;
mod router;

// Declare the ai schema to pgrx's SQL entity graph.
// This lets #[pg_extern(schema = "ai")] validate at SQL generation time.
#[pg_schema]
mod ai {}

// ----------------------------------------------------------------
// Step 1 — ai.endpoints (root of dependency chain)
// ----------------------------------------------------------------
extension_sql!(
    r#"
    CREATE TABLE ai.endpoints (
        id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
        name            text UNIQUE NOT NULL,
        service         text NOT NULL,
        base_url        text NOT NULL,
        api_key_env     text,
        health_url      text GENERATED ALWAYS AS (base_url || '/health') STORED,
        is_active       boolean NOT NULL DEFAULT true,
        last_checked_at timestamptz
    );
    SELECT pg_catalog.pg_extension_config_dump('ai.endpoints', '');
    "#,
    name = "ai_endpoints_table",
    requires = [ai],
);

// ----------------------------------------------------------------
// Step 2 — ai.models (requires endpoints)
// ----------------------------------------------------------------
extension_sql!(
    r#"
    CREATE TABLE ai.models (
        id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
        name        text UNIQUE NOT NULL,
        version     text NOT NULL DEFAULT '1.0',
        model_type  text NOT NULL,
        provider    text NOT NULL,
        endpoint_id uuid REFERENCES ai.endpoints(id),
        config      jsonb NOT NULL DEFAULT '{}',
        is_default  boolean NOT NULL DEFAULT false,
        created_at  timestamptz NOT NULL DEFAULT now()
    );
    SELECT pg_catalog.pg_extension_config_dump('ai.models', '');
    "#,
    name = "ai_models_table",
    requires = ["ai_endpoints_table"],
);

// ----------------------------------------------------------------
// Step 3 — ai.results (async job queue, ADR-006)
// pending_timeout_at: pipeline-worker marks 'error' when exceeded.
// Reaper query: UPDATE ai.results SET status='error', finished_at=now()
//   WHERE status='pending' AND now() > pending_timeout_at;
// ----------------------------------------------------------------
extension_sql!(
    r#"
    CREATE TABLE ai.results (
        id                  uuid PRIMARY KEY DEFAULT gen_random_uuid(),
        request_id          uuid NOT NULL,
        status              text NOT NULL DEFAULT 'pending',
        data                jsonb,
        error_msg           text,
        created_at          timestamptz NOT NULL DEFAULT now(),
        finished_at         timestamptz,
        pending_timeout_at  timestamptz NOT NULL DEFAULT now() + interval '5 minutes'
    );
    CREATE INDEX ai_results_pending_idx
        ON ai.results (pending_timeout_at)
        WHERE status = 'pending';
    SELECT pg_catalog.pg_extension_config_dump('ai.results', '');
    "#,
    name = "ai_results_table",
    requires = ["ai_models_table"],
);

// ----------------------------------------------------------------
// Step 4 — ai.pipelines (named RAG pipeline registry, like aidb.create_retriever)
// ----------------------------------------------------------------
extension_sql!(
    r#"
    CREATE TABLE ai.pipelines (
        id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
        name        text UNIQUE NOT NULL,
        collection  text NOT NULL DEFAULT 'default',
        embed_model text NOT NULL DEFAULT 'default',
        llm_model   text NOT NULL DEFAULT 'default-llm',
        config      jsonb NOT NULL DEFAULT '{}',
        created_at  timestamptz NOT NULL DEFAULT now()
    );
    SELECT pg_catalog.pg_extension_config_dump('ai.pipelines', '');
    "#,
    name = "ai_pipelines_table",
    requires = ["ai_results_table"],
);

// ----------------------------------------------------------------
// Step 5 — ai._outbox (ADR-001 Outbox pattern for ingest events)
// pipeline-worker LISTENs on 'aidb_pipeline' and reads payload here.
// ----------------------------------------------------------------
extension_sql!(
    r#"
    CREATE TABLE ai._outbox (
        id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
        event_type text NOT NULL DEFAULT 'ingest',
        payload    jsonb NOT NULL,
        created_at timestamptz NOT NULL DEFAULT now(),
        taken_at   timestamptz
    );
    SELECT pg_catalog.pg_extension_config_dump('ai._outbox', '');
    "#,
    name = "ai_outbox_table",
    requires = ["ai_pipelines_table"],
);

// ----------------------------------------------------------------
// Step 6 — platform_ai schema for stable v1 contract views (ADR-004)
// Internal tables in ai.* may be refactored; *_v1 views guarantee column shape.
// Consumers should query platform_ai.*_v1 instead of underlying ai.* tables.
// ----------------------------------------------------------------
extension_sql!(
    r#"
    CREATE SCHEMA platform_ai;
    COMMENT ON SCHEMA platform_ai IS
        'Stable contract surface. Internal ai.* tables may evolve;
         _v1 views keep their column shape until a _v2 is added.';
    "#,
    name = "platform_ai_schema",
    requires = ["ai_outbox_table"],
);

extension_sql!(
    r#"
    CREATE VIEW platform_ai.endpoints_v1 AS
    SELECT id, name, service, base_url, is_active, last_checked_at, health_url
    FROM ai.endpoints;
    COMMENT ON VIEW platform_ai.endpoints_v1 IS
        'Endpoint registry minus api_key_env (env var name only — values live in container env).';

    CREATE VIEW platform_ai.models_v1 AS
    SELECT m.id, m.name, m.version, m.model_type, m.provider,
           e.name AS endpoint_name,
           m.config, m.is_default, m.created_at
    FROM ai.models m
    LEFT JOIN ai.endpoints e ON e.id = m.endpoint_id;
    COMMENT ON VIEW platform_ai.models_v1 IS
        'Model registry. endpoint_name is denormalized — do not depend on internal endpoint_id.';

    CREATE VIEW platform_ai.pipelines_v1 AS
    SELECT name, collection, embed_model, llm_model, config, created_at
    FROM ai.pipelines;
    COMMENT ON VIEW platform_ai.pipelines_v1 IS
        'Named RAG pipelines. id is internal and not exposed.';

    CREATE VIEW platform_ai.results_v1 AS
    SELECT request_id,
           data->>'op'                                                AS op,
           status,
           data->>'query'                                             AS query,
           data->>'source'                                            AS source,
           data->>'pipeline'                                          AS pipeline,
           data->>'answer'                                            AS answer,
           jsonb_array_length(COALESCE(data->'results', '[]'::jsonb)) AS n_search_results,
           error_msg,
           created_at,
           finished_at,
           pending_timeout_at
    FROM ai.results;
    COMMENT ON VIEW platform_ai.results_v1 IS
        'Async results projected into stable columns. ai.results.data jsonb shape may evolve.';

    -- C4 token usage / cost — projected from data->'usage' on completed async ops.
    -- search has a flat usage shape (one embed call); ask has nested {embed, generate, total_tokens}.
    CREATE VIEW platform_ai.usage_v1 AS
    SELECT request_id,
           data->>'op'                                  AS op,
           data->>'pipeline'                            AS pipeline,
           CASE WHEN data->>'op' = 'ask'
                THEN data->'usage'->'embed'->>'model'
                ELSE data->'usage'->>'model'
           END                                          AS embed_model,
           CASE WHEN data->>'op' = 'ask'
                THEN data->'usage'->'generate'->>'model'
                ELSE NULL
           END                                          AS llm_model,
           COALESCE((data->'usage'->>'total_tokens')::int, 0) AS total_tokens,
           COALESCE((data->'usage'->'embed'->>'prompt_tokens')::int,
                    (data->'usage'->>'prompt_tokens')::int, 0) AS embed_prompt_tokens,
           COALESCE((data->'usage'->'generate'->>'prompt_tokens')::int, 0)     AS llm_prompt_tokens,
           COALESCE((data->'usage'->'generate'->>'completion_tokens')::int, 0) AS llm_completion_tokens,
           created_at, finished_at,
           EXTRACT(EPOCH FROM (finished_at - created_at))::numeric(10,3) AS duration_sec
    FROM ai.results
    WHERE status = 'done' AND data ? 'usage';
    COMMENT ON VIEW platform_ai.usage_v1 IS
        'Token usage per async request. SELECT op, sum(total_tokens) FROM platform_ai.usage_v1 GROUP BY op;';
    "#,
    name = "platform_ai_views",
    requires = ["platform_ai_schema"],
);

// ----------------------------------------------------------------
// Test helpers (shared across modules)
// ----------------------------------------------------------------
#[cfg(any(test, feature = "pg_test"))]
pub(crate) fn setup_test_pipeline(pipeline_name: &str) {
    Spi::run(&format!(
        "INSERT INTO ai.pipelines(name, collection, embed_model, llm_model) \
         VALUES ('{pipeline_name}', 'test-collection', 'default', 'default-llm') \
         ON CONFLICT (name) DO NOTHING"
    ))
    .expect("setup pipeline");
}

#[cfg(any(test, feature = "pg_test"))]
pub(crate) fn setup_test_model(ep_name: &str, model_name: &str, base_url: &str) {
    Spi::run(&format!(
        "INSERT INTO ai.endpoints(name, service, base_url) \
         VALUES ('{ep_name}', 'inference', '{base_url}') \
         ON CONFLICT (name) DO NOTHING"
    ))
    .expect("setup endpoint");
    Spi::run(&format!(
        "INSERT INTO ai.models(name, model_type, provider, endpoint_id) \
         SELECT '{model_name}', 'embedding', 'mock', id \
         FROM ai.endpoints WHERE name='{ep_name}' \
         ON CONFLICT (name) DO NOTHING"
    ))
    .expect("setup model");
}

#[cfg(any(test, feature = "pg_test"))]
#[pg_schema]
mod tests {
    use pgrx::prelude::*;

    // ----------------------------------------------------------------
    // Schema + table structure
    // ----------------------------------------------------------------

    #[pg_test]
    fn test_schema_exists() {
        let exists = Spi::get_one::<bool>(
            "SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname = 'ai')",
        )
        .expect("SPI failed")
        .unwrap_or(false);
        assert!(exists, "ai schema must exist after CREATE EXTENSION");
    }

    #[pg_test]
    fn test_tables_exist() {
        for table in &["endpoints", "models", "results"] {
            let exists = Spi::get_one::<bool>(&format!(
                "SELECT to_regclass('ai.{}') IS NOT NULL",
                table
            ))
            .expect("SPI failed")
            .unwrap_or(false);
            assert!(exists, "table ai.{} must exist", table);
        }
    }

    #[pg_test]
    fn test_results_has_8_columns() {
        let count = Spi::get_one::<i64>(
            "SELECT COUNT(*) FROM information_schema.columns \
             WHERE table_schema = 'ai' AND table_name = 'results'",
        )
        .expect("SPI failed")
        .unwrap_or(0);
        assert_eq!(count, 8, "ai.results must have exactly 8 columns");
    }

    #[pg_test]
    fn test_pending_timeout_at_default_is_5min() {
        // Insert a row and check pending_timeout_at is ~5 minutes from now
        Spi::run(
            "INSERT INTO ai.results(request_id, status) \
             VALUES (gen_random_uuid(), 'pending')",
        )
        .expect("insert");
        let ok = Spi::get_one::<bool>(
            "SELECT (pending_timeout_at - created_at) \
                    BETWEEN interval '4 minutes 55 seconds' \
                        AND interval '5 minutes 5 seconds' \
             FROM ai.results ORDER BY created_at DESC LIMIT 1",
        )
        .expect("SPI failed")
        .unwrap_or(false);
        assert!(ok, "pending_timeout_at default must be ~5 minutes from created_at");
    }

    #[pg_test]
    fn test_endpoint_health_url_is_generated() {
        Spi::run(
            "INSERT INTO ai.endpoints(name, service, base_url) \
             VALUES ('test-health-ep', 'inference', 'http://svc:8080') \
             ON CONFLICT (name) DO NOTHING",
        )
        .expect("insert endpoint");
        let health = Spi::get_one::<String>(
            "SELECT health_url FROM ai.endpoints WHERE name = 'test-health-ep'",
        )
        .expect("SPI failed");
        assert_eq!(
            health.as_deref(),
            Some("http://svc:8080/health"),
            "health_url must be base_url || '/health'"
        );
    }

    #[pg_test]
    fn test_models_fk_rejects_invalid_endpoint() {
        // Inserting a model with a non-existent endpoint_id must fail (FK violation)
        let result = std::panic::catch_unwind(|| {
            Spi::run(
                "INSERT INTO ai.models(name, model_type, provider, endpoint_id) \
                 VALUES ('bad-model', 'embedding', 'mock', gen_random_uuid())",
            )
            .expect("expected FK violation");
        });
        assert!(result.is_err(), "FK violation must be raised for invalid endpoint_id");
    }

    #[pg_test]
    fn test_functions_exist_in_ai_schema() {
        for func in &["embed_raw", "embed_async"] {
            let exists = Spi::get_one::<bool>(&format!(
                "SELECT EXISTS( \
                    SELECT 1 FROM pg_proc p \
                    JOIN pg_namespace n ON n.oid = p.pronamespace \
                    WHERE n.nspname = 'ai' AND p.proname = '{func}' \
                 )"
            ))
            .expect("SPI failed")
            .unwrap_or(false);
            assert!(exists, "function ai.{func} must exist");
        }
    }

    #[pg_test]
    fn test_embed_async_return_type_is_uuid() {
        // pg_type.typname is type 'name' not 'text'; compare as bool to avoid cast issues
        let ok = Spi::get_one::<bool>(
            "SELECT t.typname = 'uuid' \
             FROM pg_proc p \
             JOIN pg_namespace n ON n.oid = p.pronamespace \
             JOIN pg_type t      ON t.oid = p.prorettype \
             WHERE n.nspname = 'ai' AND p.proname = 'embed_async'",
        )
        .expect("SPI failed")
        .unwrap_or(false);
        assert!(ok, "embed_async must return uuid type");
    }

    #[pg_test]
    fn test_embed_raw_return_type_is_float4_array() {
        // format_type() returns text; verify embed_raw returns real[] (= float4[])
        let ok = Spi::get_one::<bool>(
            "SELECT format_type(p.prorettype, NULL) = 'real[]' \
             FROM pg_proc p \
             JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE n.nspname = 'ai' AND p.proname = 'embed_raw'",
        )
        .expect("SPI failed")
        .unwrap_or(false);
        assert!(ok, "embed_raw must return real[] (float4[]) type");
    }

    #[pg_test]
    fn test_functions_are_security_definer() {
        let count = Spi::get_one::<i64>(
            "SELECT COUNT(*) FROM pg_proc p \
             JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE n.nspname = 'ai' \
               AND p.proname IN ('embed_raw', 'embed_async') \
               AND p.prosecdef = true",
        )
        .expect("SPI failed")
        .unwrap_or(0);
        assert_eq!(count, 2, "both embed_raw and embed_async must be SECURITY DEFINER");
    }

    #[pg_test]
    fn test_functions_are_volatile() {
        let count = Spi::get_one::<i64>(
            "SELECT COUNT(*) FROM pg_proc p \
             JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE n.nspname = 'ai' \
               AND p.proname IN ('embed_raw', 'embed_async') \
               AND p.provolatile = 'v'",
        )
        .expect("SPI failed")
        .unwrap_or(0);
        assert_eq!(count, 2, "both functions must be VOLATILE");
    }

    // ----------------------------------------------------------------
    // RAG platform tables
    // ----------------------------------------------------------------

    #[pg_test]
    fn test_pipelines_table_exists() {
        let exists = Spi::get_one::<bool>(
            "SELECT to_regclass('ai.pipelines') IS NOT NULL",
        )
        .expect("SPI failed")
        .unwrap_or(false);
        assert!(exists, "ai.pipelines must exist after CREATE EXTENSION");
    }

    #[pg_test]
    fn test_outbox_table_exists() {
        let exists = Spi::get_one::<bool>(
            "SELECT to_regclass('ai._outbox') IS NOT NULL",
        )
        .expect("SPI failed")
        .unwrap_or(false);
        assert!(exists, "ai._outbox must exist after CREATE EXTENSION");
    }

    #[pg_test]
    fn test_pipelines_has_7_columns() {
        let count = Spi::get_one::<i64>(
            "SELECT COUNT(*) FROM information_schema.columns \
             WHERE table_schema = 'ai' AND table_name = 'pipelines'",
        )
        .expect("SPI failed")
        .unwrap_or(0);
        assert_eq!(count, 7, "ai.pipelines must have exactly 7 columns");
    }

    #[pg_test]
    fn test_new_functions_exist_in_ai_schema() {
        for func in &["create_pipeline", "ingest", "search", "ask", "search_async", "ask_async"] {
            let exists = Spi::get_one::<bool>(&format!(
                "SELECT EXISTS( \
                    SELECT 1 FROM pg_proc p \
                    JOIN pg_namespace n ON n.oid = p.pronamespace \
                    WHERE n.nspname = 'ai' AND p.proname = '{func}' \
                 )"
            ))
            .expect("SPI failed")
            .unwrap_or(false);
            assert!(exists, "function ai.{func} must exist");
        }
    }

    #[pg_test]
    fn test_new_functions_are_security_definer() {
        let count = Spi::get_one::<i64>(
            "SELECT COUNT(*) FROM pg_proc p \
             JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE n.nspname = 'ai' \
               AND p.proname IN ('create_pipeline','ingest','search','ask','search_async','ask_async') \
               AND p.prosecdef = true",
        )
        .expect("SPI failed")
        .unwrap_or(0);
        assert_eq!(count, 6, "all 6 RAG functions must be SECURITY DEFINER");
    }

    #[pg_test]
    fn test_platform_ai_schema_exists() {
        let exists = Spi::get_one::<bool>(
            "SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname = 'platform_ai')",
        )
        .expect("SPI failed")
        .unwrap_or(false);
        assert!(exists, "platform_ai schema must exist after CREATE EXTENSION");
    }

    #[pg_test]
    fn test_platform_ai_v1_views_exist() {
        for view in &["endpoints_v1", "models_v1", "pipelines_v1", "results_v1", "usage_v1"] {
            let exists = Spi::get_one::<bool>(&format!(
                "SELECT EXISTS( \
                    SELECT 1 FROM pg_views \
                    WHERE schemaname='platform_ai' AND viewname='{view}' \
                 )"
            ))
            .expect("SPI failed")
            .unwrap_or(false);
            assert!(exists, "platform_ai.{view} must exist");
        }
    }

    #[pg_test]
    fn test_endpoints_v1_hides_api_key_env() {
        let visible = Spi::get_one::<bool>(
            "SELECT EXISTS( \
                SELECT 1 FROM information_schema.columns \
                WHERE table_schema='platform_ai' AND table_name='endpoints_v1' \
                  AND column_name='api_key_env' \
             )",
        )
        .expect("SPI failed")
        .unwrap_or(true);
        assert!(!visible, "api_key_env must NOT be exposed by platform_ai.endpoints_v1");
    }

    #[pg_test]
    fn test_new_functions_are_volatile() {
        let count = Spi::get_one::<i64>(
            "SELECT COUNT(*) FROM pg_proc p \
             JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE n.nspname = 'ai' \
               AND p.proname IN ('create_pipeline','ingest','search','ask','search_async','ask_async') \
               AND p.provolatile = 'v'",
        )
        .expect("SPI failed")
        .unwrap_or(0);
        assert_eq!(count, 6, "all 6 RAG functions must be VOLATILE");
    }
}

#[cfg(test)]
pub mod pg_test {
    pub fn setup(_options: Vec<&str>) {}
    pub fn postgresql_conf_options() -> Vec<&'static str> {
        vec![]
    }
}
