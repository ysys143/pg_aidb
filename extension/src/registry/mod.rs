// Registry module — model + endpoint lookups via SPI (Phase 1)
// pgrx 0.18 SPI args: &[DatumWithOid<'_>]
// Use value.into() directly so the type's Into<DatumWithOid> impl sets correct OID.

use pgrx::prelude::*;

pub struct PipelineConfig {
    pub collection: String,
    pub embed_model: String,
    pub llm_model: String,
    pub top_k: i32,
}

/// Looks up a named pipeline's config from ai.pipelines.
/// Returns None if no matching row found.
pub fn lookup_pipeline(name: &str) -> Option<PipelineConfig> {
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT collection, embed_model, llm_model, \
                        COALESCE((config->>'top_k')::int, 5) \
                 FROM ai.pipelines WHERE name = $1 LIMIT 1",
                Some(1),
                &[name.into()],
            )
            .unwrap();

        let mut iter = rows.into_iter();
        match iter.next() {
            None => None,
            Some(row) => Some(PipelineConfig {
                collection: row.get::<String>(1).unwrap().unwrap(),
                embed_model: row.get::<String>(2).unwrap().unwrap(),
                llm_model: row.get::<String>(3).unwrap().unwrap(),
                top_k: row.get::<i32>(4).unwrap().unwrap_or(5),
            }),
        }
    })
}

/// Looks up (base_url, api_key_env) for the named model.
/// Returns None if no matching row found.
pub fn lookup_endpoint(model_name: &str) -> Option<(String, Option<String>)> {
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT e.base_url, e.api_key_env \
                 FROM ai.models m \
                 JOIN ai.endpoints e ON e.id = m.endpoint_id \
                 WHERE m.name = $1 \
                 LIMIT 1",
                Some(1),
                &[model_name.into()],
            )
            .unwrap();

        let mut iter = rows.into_iter();
        match iter.next() {
            None => None,
            Some(row) => {
                let base: String = row.get(1).unwrap().unwrap();
                let env: Option<String> = row.get(2).unwrap();
                Some((base, env))
            }
        }
    })
}

#[cfg(any(test, feature = "pg_test"))]
#[pg_schema]
mod tests {
    use pgrx::prelude::*;
    use crate::setup_test_model;
    use super::{lookup_endpoint, lookup_pipeline};

    // ----------------------------------------------------------------
    // lookup_pipeline
    // ----------------------------------------------------------------

    #[pg_test]
    fn test_lookup_pipeline_returns_none_for_missing() {
        let result = lookup_pipeline("nonexistent-pipeline-xyz-abc");
        assert!(result.is_none(), "missing pipeline must return None");
    }

    #[pg_test]
    fn test_lookup_pipeline_returns_config() {
        Spi::run(
            "INSERT INTO ai.pipelines(name, collection, embed_model, llm_model) \
             VALUES ('reg-cfg-pipeline', 'my-col', 'my-embed', 'my-llm') \
             ON CONFLICT (name) DO NOTHING",
        )
        .expect("setup pipeline");

        let cfg = lookup_pipeline("reg-cfg-pipeline")
            .expect("existing pipeline must return Some");
        assert_eq!(cfg.collection, "my-col");
        assert_eq!(cfg.embed_model, "my-embed");
        assert_eq!(cfg.llm_model, "my-llm");
    }

    #[pg_test]
    fn test_lookup_pipeline_default_top_k_is_5() {
        Spi::run(
            "INSERT INTO ai.pipelines(name, collection, embed_model, llm_model) \
             VALUES ('reg-default-topk', 'col', 'model', 'llm') \
             ON CONFLICT (name) DO NOTHING",
        )
        .expect("setup pipeline");

        let cfg = lookup_pipeline("reg-default-topk").expect("must return config");
        assert_eq!(cfg.top_k, 5, "default top_k must be 5 when not in config");
    }

    #[pg_test]
    fn test_lookup_pipeline_top_k_from_config() {
        Spi::run(
            "INSERT INTO ai.pipelines(name, collection, embed_model, llm_model, config) \
             VALUES ('reg-topk-pipeline', 'col', 'model', 'llm', '{\"top_k\":10}'::jsonb) \
             ON CONFLICT (name) DO NOTHING",
        )
        .expect("setup pipeline");

        let cfg = lookup_pipeline("reg-topk-pipeline").expect("must return config");
        assert_eq!(cfg.top_k, 10, "top_k must be read from config jsonb");
    }

    #[pg_test]
    fn test_lookup_endpoint_returns_none_for_missing_model() {
        let result = lookup_endpoint("nonexistent-model-xyz");
        assert!(result.is_none(), "missing model must return None");
    }

    #[pg_test]
    fn test_lookup_endpoint_returns_url_for_existing_model() {
        setup_test_model("reg-ep", "reg-model", "http://reg-svc:9001");
        let result = lookup_endpoint("reg-model");
        assert!(result.is_some(), "existing model must return Some");
        let (base_url, _) = result.unwrap();
        assert_eq!(base_url, "http://reg-svc:9001");
    }

    #[pg_test]
    fn test_lookup_endpoint_api_key_env_is_none_when_not_set() {
        setup_test_model("nokey-ep", "nokey-model", "http://nokey:9002");
        let result = lookup_endpoint("nokey-model");
        let (_, api_key_env) = result.unwrap();
        assert!(api_key_env.is_none(), "api_key_env must be None when not set");
    }

    #[pg_test]
    fn test_lookup_endpoint_returns_api_key_env_when_set() {
        Spi::run(
            "INSERT INTO ai.endpoints(name, service, base_url, api_key_env) \
             VALUES ('keyed-ep', 'inference', 'http://keyed:9003', 'MY_API_KEY') \
             ON CONFLICT (name) DO NOTHING",
        )
        .expect("setup endpoint");
        Spi::run(
            "INSERT INTO ai.models(name, model_type, provider, endpoint_id) \
             SELECT 'keyed-model', 'embedding', 'mock', id \
             FROM ai.endpoints WHERE name='keyed-ep' \
             ON CONFLICT (name) DO NOTHING",
        )
        .expect("setup model");

        let result = lookup_endpoint("keyed-model");
        let (_, api_key_env) = result.unwrap();
        assert_eq!(api_key_env.as_deref(), Some("MY_API_KEY"));
    }
}
