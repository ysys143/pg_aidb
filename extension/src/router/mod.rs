// Router module — #[pg_extern] function definitions
// pgrx 0.18 SPI args: &[DatumWithOid<'_>]
// Use value.into() directly — the type's Into<DatumWithOid> impl sets correct OID.

use pgrx::prelude::*;
use pgrx::iter::TableIterator;
use crate::{
    client::{call_ask, call_embeddings, call_search},
    registry::{lookup_endpoint, lookup_pipeline},
};

#[pg_extern(
    name = "embed_raw",
    schema = "ai",
    volatile,
    parallel_unsafe,
    security_definer
)]
fn embed_raw_impl(
    input: &str,
    model: default!(&str, "'default'"),
) -> Vec<f32> {
    // Phase 1: SPI lookup (HANDOFF.md §3)
    let (base_url, api_key_env) = lookup_endpoint(model)
        .unwrap_or_else(|| error!("ai.embed_raw: model '{}' not found in ai.models", model));

    // Phase 2: reqwest blocking HTTP — no open SPI context (HANDOFF.md §3)
    let api_key = api_key_env.and_then(|k| std::env::var(k).ok());

    call_embeddings(&base_url, api_key.as_deref(), input, model)
        .unwrap_or_else(|e| error!("ai.embed_raw: {}", e))
}

// ai.embed_async: pre-generate UUID via SQL, then INSERT + NOTIFY via
// Spi::run_with_args (read_only=false). client.select() uses read_only=true
// and rejects DML. (ADR-006, HANDOFF.md §8)
#[pg_extern(
    name = "embed_async",
    schema = "ai",
    volatile,
    parallel_unsafe,
    security_definer
)]
fn embed_async_impl(
    input: &str,
    model: default!(&str, "'default'"),
) -> pgrx::Uuid {
    let uuid: pgrx::Uuid = Spi::get_one("SELECT gen_random_uuid()")
        .expect("SPI get_one failed")
        .expect("gen_random_uuid() returned NULL");

    Spi::run_with_args(
        "INSERT INTO ai.results (request_id, status, data) \
         VALUES ($1, 'pending', \
                 jsonb_build_object('op','embed','input',$2,'model',$3))",
        &[uuid.into(), input.into(), model.into()],
    )
    .unwrap_or_else(|e| error!("ai.embed_async INSERT: {:?}", e));

    // Per-request NOTIFY channel: "ai_" + 36-char UUID = 39 chars < NAMEDATALEN(64)
    Spi::run_with_args(
        "SELECT pg_notify('ai_' || $1::text, $1::text)",
        &[uuid.into()],
    )
    .unwrap_or_else(|e| error!("ai.embed_async NOTIFY: {:?}", e));

    uuid
}

// ----------------------------------------------------------------
// ai.create_pipeline — upsert a named RAG pipeline (like aidb.create_retriever)
// ----------------------------------------------------------------
#[pg_extern(
    name = "create_pipeline",
    schema = "ai",
    volatile,
    parallel_unsafe,
    security_definer
)]
fn create_pipeline_impl(
    name: &str,
    collection: default!(&str, "'default'"),
    embed_model: default!(&str, "'default'"),
    llm_model: default!(&str, "'default-llm'"),
    config: default!(pgrx::JsonB, "'{}'"),
) {
    Spi::run_with_args(
        "INSERT INTO ai.pipelines(name, collection, embed_model, llm_model, config) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (name) DO UPDATE SET \
             collection  = EXCLUDED.collection, \
             embed_model = EXCLUDED.embed_model, \
             llm_model   = EXCLUDED.llm_model, \
             config      = EXCLUDED.config",
        &[name.into(), collection.into(), embed_model.into(), llm_model.into(), config.into()],
    )
    .unwrap_or_else(|e| error!("ai.create_pipeline: {:?}", e));
}

// ----------------------------------------------------------------
// ai.ingest — queue a document for ingestion (ADR-001 Outbox pattern)
// Returns request_id for polling ai.results.
// ----------------------------------------------------------------
#[pg_extern(
    name = "ingest",
    schema = "ai",
    volatile,
    parallel_unsafe,
    security_definer
)]
fn ingest_impl(
    source: &str,
    content: default!(&str, "''"),
    pipeline: default!(&str, "'default'"),
) -> pgrx::Uuid {
    let pipeline_cfg = lookup_pipeline(pipeline)
        .unwrap_or_else(|| error!("ai.ingest: pipeline '{}' not found in ai.pipelines", pipeline));

    // Pre-generate request_id (Phase 1, inside SPI context)
    let uuid: pgrx::Uuid = Spi::get_one("SELECT gen_random_uuid()")
        .expect("SPI get_one failed")
        .expect("gen_random_uuid() returned NULL");

    // Track async status in ai.results (same pattern as embed_async)
    Spi::run_with_args(
        "INSERT INTO ai.results(request_id, status, data) \
         VALUES ($1, 'pending', \
                 jsonb_build_object('op','ingest','source',$2,'pipeline',$3))",
        &[uuid.into(), source.into(), pipeline.into()],
    )
    .unwrap_or_else(|e| error!("ai.ingest results INSERT: {:?}", e));

    // Build outbox payload via SQL. Subquery includes the pipeline's full config jsonb
    // so the pipeline-worker can read chunking strategy and other knobs.
    Spi::run_with_args(
        "INSERT INTO ai._outbox(event_type, payload) \
         VALUES ('ingest', jsonb_build_object(\
             'request_id', $1::text, \
             'source',     $2, \
             'content',    $3, \
             'collection', $4, \
             'embed_model',$5, \
             'llm_model',  $6, \
             'config',     (SELECT config FROM ai.pipelines WHERE name = $7) \
         ))",
        &[
            uuid.into(),
            source.into(),
            content.into(),
            pipeline_cfg.collection.as_str().into(),
            pipeline_cfg.embed_model.as_str().into(),
            pipeline_cfg.llm_model.as_str().into(),
            pipeline.into(),
        ],
    )
    .unwrap_or_else(|e| error!("ai.ingest outbox INSERT: {:?}", e));

    // NOTIFY pipeline-worker (NOTIFY-as-hint, ai._outbox is truth)
    Spi::run_with_args(
        "SELECT pg_notify('aidb_pipeline', $1::text)",
        &[uuid.into()],
    )
    .unwrap_or_else(|e| error!("ai.ingest NOTIFY: {:?}", e));

    uuid
}

// ----------------------------------------------------------------
// ai.search — retrieval only (like aidb.retrieve)
// Returns TABLE of matching chunks with similarity scores.
// ----------------------------------------------------------------
#[pg_extern(
    name = "search",
    schema = "ai",
    volatile,
    parallel_unsafe,
    security_definer
)]
fn search_impl(
    query: &str,
    pipeline: default!(&str, "'default'"),
    top_k: default!(i32, "0"),
    filter: default!(pgrx::JsonB, "'{}'"),
) -> TableIterator<
    'static,
    (
        name!(chunk_id, String),
        name!(content, String),
        name!(similarity, f32),
        name!(source, String),
        name!(metadata, pgrx::JsonB),
    ),
> {
    // Phase 1: resolve pipeline + endpoint (SPI, closes before HTTP)
    let pipeline_cfg = lookup_pipeline(pipeline)
        .unwrap_or_else(|| error!("ai.search: pipeline '{}' not found in ai.pipelines", pipeline));

    let effective_top_k = if top_k > 0 { top_k } else { pipeline_cfg.top_k };

    let (base_url, api_key_env) = lookup_endpoint(&pipeline_cfg.embed_model)
        .unwrap_or_else(|| {
            error!(
                "ai.search: embed_model '{}' not found in ai.models",
                pipeline_cfg.embed_model
            )
        });

    // Phase 2: HTTP call to rag service (outside SPI)
    let api_key = api_key_env.and_then(|k| std::env::var(k).ok());

    let results = call_search(
        &base_url,
        api_key.as_deref(),
        query,
        &pipeline_cfg.collection,
        effective_top_k,
        &filter.0,
    )
    .unwrap_or_else(|e| error!("ai.search: {}", e));

    let rows: Vec<(String, String, f32, String, pgrx::JsonB)> = results
        .into_iter()
        .map(|r| (r.chunk_id, r.content, r.similarity, r.source, pgrx::JsonB(r.metadata)))
        .collect();

    TableIterator::new(rows)
}

// ----------------------------------------------------------------
// ai.ask — retrieval + LLM generation (like aidb.retrieve_and_generate)
// Returns LLM-generated answer grounded in retrieved chunks.
// ----------------------------------------------------------------
#[pg_extern(
    name = "ask",
    schema = "ai",
    volatile,
    parallel_unsafe,
    security_definer
)]
fn ask_impl(
    query: &str,
    pipeline: default!(&str, "'default'"),
) -> String {
    // Phase 1: resolve pipeline + endpoint (SPI)
    let pipeline_cfg = lookup_pipeline(pipeline)
        .unwrap_or_else(|| error!("ai.ask: pipeline '{}' not found in ai.pipelines", pipeline));

    let (base_url, api_key_env) = lookup_endpoint(&pipeline_cfg.embed_model)
        .unwrap_or_else(|| {
            error!(
                "ai.ask: embed_model '{}' not found in ai.models",
                pipeline_cfg.embed_model
            )
        });

    // Phase 2: HTTP call to rag service /ask endpoint (outside SPI)
    let api_key = api_key_env.and_then(|k| std::env::var(k).ok());

    call_ask(
        &base_url,
        api_key.as_deref(),
        query,
        &pipeline_cfg.collection,
        pipeline_cfg.top_k,
    )
    .unwrap_or_else(|e| error!("ai.ask: {}", e))
}

// ----------------------------------------------------------------
// ai.search_async — async retrieval. Returns request_id immediately;
// pipeline-worker writes results jsonb to ai.results.data.
// ----------------------------------------------------------------
#[pg_extern(
    name = "search_async",
    schema = "ai",
    volatile,
    parallel_unsafe,
    security_definer
)]
fn search_async_impl(
    query: &str,
    pipeline: default!(&str, "'default'"),
    top_k: default!(i32, "0"),
) -> pgrx::Uuid {
    let pipeline_cfg = lookup_pipeline(pipeline)
        .unwrap_or_else(|| error!("ai.search_async: pipeline '{}' not found", pipeline));

    let effective_top_k = if top_k > 0 { top_k } else { pipeline_cfg.top_k };

    let uuid: pgrx::Uuid = Spi::get_one("SELECT gen_random_uuid()")
        .expect("SPI get_one failed")
        .expect("gen_random_uuid() returned NULL");

    Spi::run_with_args(
        "INSERT INTO ai.results(request_id, status, data) \
         VALUES ($1, 'pending', \
                 jsonb_build_object('op','search','query',$2,'pipeline',$3))",
        &[uuid.into(), query.into(), pipeline.into()],
    )
    .unwrap_or_else(|e| error!("ai.search_async results INSERT: {:?}", e));

    Spi::run_with_args(
        "INSERT INTO ai._outbox(event_type, payload) \
         VALUES ('search', jsonb_build_object(\
             'request_id', $1::text, \
             'query',      $2, \
             'collection', $3, \
             'top_k',      $4 \
         ))",
        &[
            uuid.into(),
            query.into(),
            pipeline_cfg.collection.as_str().into(),
            effective_top_k.into(),
        ],
    )
    .unwrap_or_else(|e| error!("ai.search_async outbox INSERT: {:?}", e));

    Spi::run_with_args(
        "SELECT pg_notify('aidb_pipeline', $1::text)",
        &[uuid.into()],
    )
    .unwrap_or_else(|e| error!("ai.search_async NOTIFY: {:?}", e));

    uuid
}

// ----------------------------------------------------------------
// ai.ask_async — async RAG. Returns request_id immediately;
// pipeline-worker writes generated answer to ai.results.data.
// ----------------------------------------------------------------
#[pg_extern(
    name = "ask_async",
    schema = "ai",
    volatile,
    parallel_unsafe,
    security_definer
)]
fn ask_async_impl(
    query: &str,
    pipeline: default!(&str, "'default'"),
) -> pgrx::Uuid {
    let pipeline_cfg = lookup_pipeline(pipeline)
        .unwrap_or_else(|| error!("ai.ask_async: pipeline '{}' not found", pipeline));

    let uuid: pgrx::Uuid = Spi::get_one("SELECT gen_random_uuid()")
        .expect("SPI get_one failed")
        .expect("gen_random_uuid() returned NULL");

    Spi::run_with_args(
        "INSERT INTO ai.results(request_id, status, data) \
         VALUES ($1, 'pending', \
                 jsonb_build_object('op','ask','query',$2,'pipeline',$3))",
        &[uuid.into(), query.into(), pipeline.into()],
    )
    .unwrap_or_else(|e| error!("ai.ask_async results INSERT: {:?}", e));

    Spi::run_with_args(
        "INSERT INTO ai._outbox(event_type, payload) \
         VALUES ('ask', jsonb_build_object(\
             'request_id', $1::text, \
             'query',      $2, \
             'collection', $3, \
             'top_k',      $4 \
         ))",
        &[
            uuid.into(),
            query.into(),
            pipeline_cfg.collection.as_str().into(),
            pipeline_cfg.top_k.into(),
        ],
    )
    .unwrap_or_else(|e| error!("ai.ask_async outbox INSERT: {:?}", e));

    Spi::run_with_args(
        "SELECT pg_notify('aidb_pipeline', $1::text)",
        &[uuid.into()],
    )
    .unwrap_or_else(|e| error!("ai.ask_async NOTIFY: {:?}", e));

    uuid
}

// pgrx 0.18 does not support `set = "..."` in #[pg_extern].
// Pin search_path via ALTER FUNCTION after all functions exist.
// public required: pgvector's vector type lives in public schema (HANDOFF.md §2:122).
extension_sql!(
    r#"
    ALTER FUNCTION ai.embed_raw(text, text)
        SET search_path = pg_catalog, public, ai, pg_temp;
    ALTER FUNCTION ai.embed_async(text, text)
        SET search_path = pg_catalog, public, ai, pg_temp;
    ALTER FUNCTION ai.create_pipeline(text, text, text, text, jsonb)
        SET search_path = pg_catalog, public, ai, pg_temp;
    ALTER FUNCTION ai.ingest(text, text, text)
        SET search_path = pg_catalog, public, ai, pg_temp;
    ALTER FUNCTION ai.search(text, text, integer, jsonb)
        SET search_path = pg_catalog, public, ai, pg_temp;
    ALTER FUNCTION ai.ask(text, text)
        SET search_path = pg_catalog, public, ai, pg_temp;
    ALTER FUNCTION ai.search_async(text, text, integer)
        SET search_path = pg_catalog, public, ai, pg_temp;
    ALTER FUNCTION ai.ask_async(text, text)
        SET search_path = pg_catalog, public, ai, pg_temp;
    "#,
    name = "ai_function_search_paths",
    requires = [
        embed_raw_impl,
        embed_async_impl,
        create_pipeline_impl,
        ingest_impl,
        search_impl,
        ask_impl,
        search_async_impl,
        ask_async_impl,
    ],
);

#[cfg(any(test, feature = "pg_test"))]
#[pg_schema]
mod tests {
    use pgrx::prelude::*;
    use crate::{setup_test_model, setup_test_pipeline};
    use super::{embed_async_impl, embed_raw_impl, create_pipeline_impl, ingest_impl, search_impl, ask_impl, search_async_impl, ask_async_impl};

    // Call Rust fns directly — never via Spi::get_one("SELECT ai.embed_async(...)")
    // #[pg_test] is SPI level 1; a SELECT wrapper → level 3 → 0 rows. (HANDOFF.md §1)

    fn setup_async_model() {
        setup_test_model("async-ep", "async-model", "http://async-svc:9999");
    }

    // ----------------------------------------------------------------
    // embed_async — happy path
    // ----------------------------------------------------------------

    #[pg_test]
    fn test_embed_async_inserts_pending_row() {
        setup_async_model();
        let _uuid = embed_async_impl("hello", "async-model");
        let count = Spi::get_one::<i64>("SELECT COUNT(*) FROM ai.results")
            .expect("SPI failed")
            .unwrap_or(0);
        assert!(count > 0, "must have at least one row in ai.results");
    }

    #[pg_test]
    fn test_embed_async_row_status_is_pending() {
        setup_async_model();
        let uuid = embed_async_impl("hello", "async-model");
        let status = Spi::get_one::<String>(&format!(
            "SELECT status FROM ai.results WHERE request_id = '{uuid}'"
        ))
        .expect("SPI failed");
        assert_eq!(status.as_deref(), Some("pending"));
    }

    #[pg_test]
    fn test_embed_async_data_contains_op_and_input() {
        setup_async_model();
        let uuid = embed_async_impl("test input", "async-model");
        let op = Spi::get_one::<String>(&format!(
            "SELECT data->>'op' FROM ai.results WHERE request_id = '{uuid}'"
        ))
        .expect("SPI failed");
        assert_eq!(op.as_deref(), Some("embed"));

        let inp = Spi::get_one::<String>(&format!(
            "SELECT data->>'input' FROM ai.results WHERE request_id = '{uuid}'"
        ))
        .expect("SPI failed");
        assert_eq!(inp.as_deref(), Some("test input"));
    }

    #[pg_test]
    fn test_embed_async_pending_timeout_at_is_set() {
        setup_async_model();
        let uuid = embed_async_impl("hello", "async-model");
        let ok = Spi::get_one::<bool>(&format!(
            "SELECT (pending_timeout_at - created_at) \
                    BETWEEN interval '4 minutes 55 seconds' \
                        AND interval '5 minutes 5 seconds' \
             FROM ai.results WHERE request_id = '{uuid}'"
        ))
        .expect("SPI failed")
        .unwrap_or(false);
        assert!(ok, "pending_timeout_at must be ~5 min from created_at");
    }

    #[pg_test]
    fn test_embed_async_finished_at_is_null_initially() {
        setup_async_model();
        let uuid = embed_async_impl("hello", "async-model");
        let finished = Spi::get_one::<bool>(&format!(
            "SELECT finished_at IS NULL FROM ai.results WHERE request_id = '{uuid}'"
        ))
        .expect("SPI failed")
        .unwrap_or(false);
        assert!(finished, "finished_at must be NULL on initial insert");
    }

    // ----------------------------------------------------------------
    // embed_async — error path
    // ----------------------------------------------------------------

    #[pg_test]
    #[should_panic]
    fn test_embed_raw_panics_on_missing_model() {
        // embed_raw calls lookup_endpoint → None → error!() → panic
        // We call directly to avoid SPI nesting trap
        embed_raw_impl("text", "nonexistent-model-xyz-abc");
    }

    // ----------------------------------------------------------------
    // create_pipeline
    // ----------------------------------------------------------------

    #[pg_test]
    fn test_create_pipeline_inserts_row() {
        create_pipeline_impl(
            "cp-test",
            "default",
            "default",
            "default-llm",
            pgrx::JsonB(serde_json::json!({"top_k": 3})),
        );
        let count = Spi::get_one::<i64>(
            "SELECT COUNT(*) FROM ai.pipelines WHERE name = 'cp-test'",
        )
        .expect("SPI failed")
        .unwrap_or(0);
        assert_eq!(count, 1, "create_pipeline must insert exactly one row");
    }

    #[pg_test]
    fn test_create_pipeline_is_idempotent() {
        create_pipeline_impl(
            "idem-test", "col1", "m1", "l1",
            pgrx::JsonB(serde_json::json!({})),
        );
        create_pipeline_impl(
            "idem-test", "col2", "m2", "l2",
            pgrx::JsonB(serde_json::json!({})),
        );
        let collection = Spi::get_one::<String>(
            "SELECT collection FROM ai.pipelines WHERE name = 'idem-test'",
        )
        .expect("SPI failed");
        assert_eq!(collection.as_deref(), Some("col2"), "second call must update collection");

        let count = Spi::get_one::<i64>(
            "SELECT COUNT(*) FROM ai.pipelines WHERE name = 'idem-test'",
        )
        .expect("SPI failed")
        .unwrap_or(0);
        assert_eq!(count, 1, "must remain exactly one row after two upserts");
    }

    // ----------------------------------------------------------------
    // ingest
    // ----------------------------------------------------------------

    #[pg_test]
    fn test_ingest_returns_uuid() {
        setup_test_pipeline("ingest-uuid-pl");
        let _uuid = ingest_impl("doc.pdf", "", "ingest-uuid-pl");
        // reaching here without panic means uuid was returned
    }

    #[pg_test]
    fn test_ingest_creates_pending_result() {
        setup_test_pipeline("ingest-result-pl");
        let before = Spi::get_one::<i64>(
            "SELECT COUNT(*) FROM ai.results \
             WHERE status = 'pending' AND data->>'op' = 'ingest'",
        )
        .expect("SPI failed")
        .unwrap_or(0);

        let _ = ingest_impl("doc.pdf", "", "ingest-result-pl");

        let after = Spi::get_one::<i64>(
            "SELECT COUNT(*) FROM ai.results \
             WHERE status = 'pending' AND data->>'op' = 'ingest'",
        )
        .expect("SPI failed")
        .unwrap_or(0);
        assert_eq!(after, before + 1, "ingest must create a pending ai.results row");
    }

    #[pg_test]
    fn test_ingest_creates_outbox_row() {
        setup_test_pipeline("ingest-outbox-pl");
        let before = Spi::get_one::<i64>(
            "SELECT COUNT(*) FROM ai._outbox WHERE event_type = 'ingest'",
        )
        .expect("SPI failed")
        .unwrap_or(0);

        let _ = ingest_impl("my-source.pdf", "", "ingest-outbox-pl");

        let after = Spi::get_one::<i64>(
            "SELECT COUNT(*) FROM ai._outbox WHERE event_type = 'ingest'",
        )
        .expect("SPI failed")
        .unwrap_or(0);
        assert_eq!(after, before + 1, "ingest must create an ai._outbox row");
    }

    #[pg_test]
    fn test_ingest_outbox_payload_contains_source() {
        setup_test_pipeline("ingest-payload-pl");
        let _ = ingest_impl("payload-test-source.pdf", "", "ingest-payload-pl");

        let source = Spi::get_one::<String>(
            "SELECT payload->>'source' FROM ai._outbox \
             WHERE event_type = 'ingest' ORDER BY created_at DESC LIMIT 1",
        )
        .expect("SPI failed");
        assert_eq!(source.as_deref(), Some("payload-test-source.pdf"));
    }

    #[pg_test]
    #[should_panic]
    fn test_ingest_panics_on_missing_pipeline() {
        ingest_impl("doc.pdf", "", "nonexistent-pipeline-xyz-abc");
    }

    // ----------------------------------------------------------------
    // search — error paths (HTTP happy path covered by installcheck-mock)
    // ----------------------------------------------------------------

    #[pg_test]
    #[should_panic]
    fn test_search_panics_on_missing_pipeline() {
        let _ = search_impl("query", "nonexistent-pipeline-xyz-abc", 5);
    }

    #[pg_test]
    #[should_panic]
    fn test_search_panics_on_missing_embed_model() {
        Spi::run(
            "INSERT INTO ai.pipelines(name, collection, embed_model, llm_model) \
             VALUES ('search-nomodel-pl', 'col', 'no-such-model-xyz', 'llm') \
             ON CONFLICT (name) DO NOTHING",
        )
        .expect("setup");
        let _ = search_impl("query", "search-nomodel-pl", 5);
    }

    // ----------------------------------------------------------------
    // ask — error paths (HTTP happy path covered by installcheck-mock)
    // ----------------------------------------------------------------

    #[pg_test]
    #[should_panic]
    fn test_ask_panics_on_missing_pipeline() {
        let _ = ask_impl("query", "nonexistent-pipeline-xyz-abc");
    }

    #[pg_test]
    #[should_panic]
    fn test_ask_panics_on_missing_embed_model() {
        Spi::run(
            "INSERT INTO ai.pipelines(name, collection, embed_model, llm_model) \
             VALUES ('ask-nomodel-pl', 'col', 'no-such-model-xyz', 'llm') \
             ON CONFLICT (name) DO NOTHING",
        )
        .expect("setup");
        let _ = ask_impl("query", "ask-nomodel-pl");
    }

    // ----------------------------------------------------------------
    // search_async — async pattern (returns uuid, queues outbox event)
    // ----------------------------------------------------------------

    #[pg_test]
    fn test_search_async_returns_uuid() {
        setup_test_pipeline("search-async-uuid-pl");
        let _uuid = search_async_impl("query", "search-async-uuid-pl", 0);
    }

    #[pg_test]
    fn test_search_async_creates_pending_result() {
        setup_test_pipeline("search-async-result-pl");
        let before = Spi::get_one::<i64>(
            "SELECT COUNT(*) FROM ai.results \
             WHERE status = 'pending' AND data->>'op' = 'search'",
        ).expect("SPI failed").unwrap_or(0);

        let _ = search_async_impl("query", "search-async-result-pl", 0);

        let after = Spi::get_one::<i64>(
            "SELECT COUNT(*) FROM ai.results \
             WHERE status = 'pending' AND data->>'op' = 'search'",
        ).expect("SPI failed").unwrap_or(0);
        assert_eq!(after, before + 1, "search_async must create a pending ai.results row");
    }

    #[pg_test]
    fn test_search_async_creates_outbox_row() {
        setup_test_pipeline("search-async-outbox-pl");
        let before = Spi::get_one::<i64>(
            "SELECT COUNT(*) FROM ai._outbox WHERE event_type = 'search'",
        ).expect("SPI failed").unwrap_or(0);

        let _ = search_async_impl("query", "search-async-outbox-pl", 0);

        let after = Spi::get_one::<i64>(
            "SELECT COUNT(*) FROM ai._outbox WHERE event_type = 'search'",
        ).expect("SPI failed").unwrap_or(0);
        assert_eq!(after, before + 1, "search_async must create an ai._outbox row");
    }

    #[pg_test]
    #[should_panic]
    fn test_search_async_panics_on_missing_pipeline() {
        let _ = search_async_impl("q", "nonexistent-pipeline-xyz-abc", 0);
    }

    // ----------------------------------------------------------------
    // ask_async
    // ----------------------------------------------------------------

    #[pg_test]
    fn test_ask_async_returns_uuid() {
        setup_test_pipeline("ask-async-uuid-pl");
        let _uuid = ask_async_impl("query", "ask-async-uuid-pl");
    }

    #[pg_test]
    fn test_ask_async_creates_outbox_row() {
        setup_test_pipeline("ask-async-outbox-pl");
        let before = Spi::get_one::<i64>(
            "SELECT COUNT(*) FROM ai._outbox WHERE event_type = 'ask'",
        ).expect("SPI failed").unwrap_or(0);

        let _ = ask_async_impl("query", "ask-async-outbox-pl");

        let after = Spi::get_one::<i64>(
            "SELECT COUNT(*) FROM ai._outbox WHERE event_type = 'ask'",
        ).expect("SPI failed").unwrap_or(0);
        assert_eq!(after, before + 1, "ask_async must create an ai._outbox row");
    }

    #[pg_test]
    #[should_panic]
    fn test_ask_async_panics_on_missing_pipeline() {
        let _ = ask_async_impl("q", "nonexistent-pipeline-xyz-abc");
    }

    // ----------------------------------------------------------------
    // embed_raw — structure (HTTP path covered by installcheck-mock)
    // ----------------------------------------------------------------

    #[pg_test]
    fn test_embed_raw_is_registered_with_correct_arity() {
        let arity = Spi::get_one::<i16>(
            "SELECT pronargs FROM pg_proc p \
             JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE n.nspname = 'ai' AND p.proname = 'embed_raw'",
        )
        .expect("SPI failed")
        .unwrap_or(0);
        assert_eq!(arity, 2, "embed_raw must take 2 arguments (input, model)");
    }
}
