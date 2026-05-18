use pgrx::prelude::*;

pg_module_magic!();

pub mod client;
pub mod registry;
pub mod router;

// ── Schema ──────────────────────────────────────────────────────────────────

extension_sql!(
    r#"CREATE SCHEMA IF NOT EXISTS ai;"#,
    name = "ai_schema",
    bootstrap
);

// ── Async result store ───────────────────────────────────────────────────────
//
// Single source of truth for all async operations.
// NOTIFY is a hint; this table is the authoritative record.

extension_sql!(
    r#"
    CREATE TABLE ai.results (
        id          uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
        status      text        NOT NULL DEFAULT 'pending'
                                CHECK (status IN ('pending','running','done','error')),
        data        jsonb,
        error_msg   text,
        created_at  timestamptz NOT NULL DEFAULT now(),
        finished_at timestamptz
    );
    "#,
    name = "ai_results_table",
    requires = ["ai_schema"]
);

// ── Model registry ───────────────────────────────────────────────────────────

extension_sql!(
    r#"
    CREATE TABLE ai.endpoints (
        id          uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
        name        text        UNIQUE NOT NULL,
        service     text        NOT NULL
                                CHECK (service IN ('inference','rag','nl2sql','pipeline')),
        base_url    text        NOT NULL,
        api_key_env text,
        is_active   boolean     NOT NULL DEFAULT true,
        last_checked_at timestamptz
    );

    CREATE TABLE ai.models (
        id          uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
        name        text        UNIQUE NOT NULL,
        model_type  text        NOT NULL
                                CHECK (model_type IN ('embedding','llm','reranker','onnx')),
        provider    text        NOT NULL,
        endpoint_id uuid        REFERENCES ai.endpoints(id),
        config      jsonb       NOT NULL DEFAULT '{}',
        is_default  boolean     NOT NULL DEFAULT false,
        created_at  timestamptz NOT NULL DEFAULT now()
    );
    "#,
    name = "ai_registry_tables",
    requires = ["ai_schema"]
);

// ── Registry management functions ────────────────────────────────────────────

extension_sql!(
    r#"
    CREATE FUNCTION ai.register_endpoint(
        p_name        text,
        p_service     text,
        p_base_url    text,
        p_api_key_env text DEFAULT NULL
    ) RETURNS uuid
    LANGUAGE sql
    SECURITY DEFINER
    SET search_path = pg_catalog, ai, pg_temp
    AS $$
        INSERT INTO ai.endpoints (name, service, base_url, api_key_env)
        VALUES (p_name, p_service, p_base_url, p_api_key_env)
        ON CONFLICT (name) DO UPDATE SET
            base_url    = EXCLUDED.base_url,
            api_key_env = EXCLUDED.api_key_env,
            is_active   = true
        RETURNING id;
    $$;

    CREATE FUNCTION ai.register_model(
        p_name        text,
        p_model_type  text,
        p_provider    text,
        p_endpoint    text,
        p_config      jsonb    DEFAULT '{}',
        p_is_default  boolean  DEFAULT false
    ) RETURNS uuid
    LANGUAGE sql
    SECURITY DEFINER
    SET search_path = pg_catalog, ai, pg_temp
    AS $$
        INSERT INTO ai.models (name, model_type, provider, endpoint_id, config, is_default)
        SELECT p_name, p_model_type, p_provider, e.id, p_config, p_is_default
        FROM   ai.endpoints e WHERE e.name = p_endpoint
        ON CONFLICT (name) DO UPDATE SET
            config     = EXCLUDED.config,
            is_default = EXCLUDED.is_default
        RETURNING id;
    $$;
    "#,
    name = "ai_registry_functions",
    requires = ["ai_registry_tables"]
);

#[cfg(any(test, feature = "pg_test"))]
#[pg_schema]
mod tests {
    use pgrx::prelude::*;

    #[pg_test]
    fn test_schema_exists() {
        let exists = Spi::get_one::<bool>(
            "SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname = 'ai')"
        ).unwrap().unwrap();
        assert!(exists);
    }

    #[pg_test]
    fn test_results_table_exists() {
        let exists = Spi::get_one::<bool>(
            "SELECT EXISTS(SELECT 1 FROM pg_tables WHERE schemaname='ai' AND tablename='results')"
        ).unwrap().unwrap();
        assert!(exists);
    }

    #[pg_test]
    fn test_register_endpoint() {
        Spi::run(
            "SELECT ai.register_endpoint('test-ep','inference','http://localhost:8001')"
        ).unwrap();
        let count = Spi::get_one::<i64>(
            "SELECT count(*) FROM ai.endpoints WHERE name = 'test-ep'"
        ).unwrap().unwrap();
        assert_eq!(count, 1);
    }
}

#[cfg(test)]
pub mod pg_test {
    pub fn setup(_options: Vec<&str>) {}
    pub fn postgresql_conf_options() -> Vec<&'static str> { vec![] }
}
