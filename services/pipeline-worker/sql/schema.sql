-- Run by pipeline-worker on startup (idempotent).
-- pgvector must be installed in the pg image (pgvector/pgvector:pg17).
-- ai schema may already exist (created by pg_aidb extension); IF NOT EXISTS is safe.

-- pgvector must be enabled (pgvector/pgvector:pg17 image includes it).
-- ai schema is created by pg_aidb extension; pipeline-worker only adds its own tables.
CREATE EXTENSION IF NOT EXISTS vector;

-- textsearch_ko: Korean MeCab tsvector; registers 'public.korean' text search config.
-- pg_textsearch: BM25 ranking + bm25 access method (requires shared_preload_libraries).
CREATE EXTENSION IF NOT EXISTS textsearch_ko;
CREATE EXTENSION IF NOT EXISTS pg_textsearch;

CREATE TABLE IF NOT EXISTS ai.documents (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    collection text NOT NULL DEFAULT 'default',
    source     text NOT NULL,
    content    text NOT NULL,
    metadata   jsonb NOT NULL DEFAULT '{}',
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS ai.chunks (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    document_id     uuid REFERENCES ai.documents(id) ON DELETE CASCADE,
    parent_chunk_id uuid REFERENCES ai.chunks(id),
    collection      text NOT NULL DEFAULT 'default',
    content         text NOT NULL,
    chunk_index     int NOT NULL,
    embedding       vector(1536),
    metadata        jsonb NOT NULL DEFAULT '{}',
    created_at      timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS ai_chunks_embedding_idx
    ON ai.chunks USING hnsw (embedding vector_cosine_ops);

CREATE INDEX IF NOT EXISTS ai_chunks_collection_idx
    ON ai.chunks (collection);

-- A7 hybrid search: Korean tsvector + BM25.
-- to_tsvector(regconfig, text) is IMMUTABLE (text-form is only STABLE),
-- so the explicit ::regconfig cast is required for a STORED generated column.
ALTER TABLE ai.chunks
    ADD COLUMN IF NOT EXISTS content_tsv tsvector
    GENERATED ALWAYS AS (to_tsvector('public.korean'::regconfig, content)) STORED;

CREATE INDEX IF NOT EXISTS ai_chunks_content_tsv_idx
    ON ai.chunks USING gin (content_tsv);

-- pg_textsearch BM25 index. text_config matches textsearch_ko's 'public.korean'.
-- The bm25 access method ranks via the `text <@> query` operator.
CREATE INDEX IF NOT EXISTS ai_chunks_bm25_idx
    ON ai.chunks USING bm25 (content) WITH (text_config = 'public.korean');

CREATE INDEX IF NOT EXISTS ai_documents_collection_idx
    ON ai.documents (collection);

-- platform_ai contract views for pipeline-worker-owned tables (ADR-004).
-- Created with CREATE OR REPLACE because the schema may be recreated by the
-- extension on each install; views are idempotent.
CREATE OR REPLACE VIEW platform_ai.documents_v1 AS
SELECT id, collection, source, content, metadata, created_at
FROM ai.documents;
COMMENT ON VIEW platform_ai.documents_v1 IS
    'Ingested documents. content is the full extracted text.';

CREATE OR REPLACE VIEW platform_ai.chunks_v1 AS
SELECT c.id, c.document_id, d.source AS document_source,
       c.collection, c.chunk_index, c.content, c.parent_chunk_id,
       c.metadata, c.created_at
FROM ai.chunks c
LEFT JOIN ai.documents d ON d.id = c.document_id;
COMMENT ON VIEW platform_ai.chunks_v1 IS
    'Chunks with their source document. embedding vector is intentionally hidden — query ai.chunks directly if you need it.';

-- A7: hybrid retrieval — dense (ai.search) + sparse (BM25 via pg_textsearch),
-- fused with Reciprocal Rank Fusion. Lives in schema.sql (not the extension)
-- because it depends on pg_textsearch (preloaded) and ai.chunks (this file),
-- both of which only exist after this script runs.
CREATE OR REPLACE FUNCTION ai.search_hybrid(
    query    text,
    pipeline text DEFAULT 'default',
    top_k    int  DEFAULT 5,
    rrf_k    int  DEFAULT 60,
    filter   jsonb DEFAULT '{}'
)
RETURNS TABLE (
    chunk_id   text,
    content    text,
    score      float,
    source     text,
    metadata   jsonb
)
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = ai, public
AS $$
#variable_conflict use_column
DECLARE
    coll text;
BEGIN
    SELECT collection INTO coll FROM ai.pipelines WHERE name = pipeline;
    IF coll IS NULL THEN
        RAISE EXCEPTION 'ai.search_hybrid: pipeline % not found', pipeline;
    END IF;

    RETURN QUERY
    WITH dense AS (
        SELECT s.chunk_id, s.content, s.source, s.metadata,
               row_number() OVER (ORDER BY s.similarity DESC) AS rnk
        FROM ai.search(query, pipeline, top_k * 4, filter) s
    ),
    sparse AS (
        SELECT c.id::text AS chunk_id,
               c.content,
               d.source,
               c.metadata,
               row_number() OVER (
                   ORDER BY c.content <@> to_bm25query(query, 'ai_chunks_bm25_idx')
               ) AS rnk
        FROM ai.chunks c
        JOIN ai.documents d ON d.id = c.document_id
        WHERE c.collection = coll
          AND c.content_tsv @@ plainto_tsquery('public.korean'::regconfig, query)
          AND (filter = '{}'::jsonb OR c.metadata @> filter)
        LIMIT top_k * 4
    ),
    fused AS (
        SELECT chunk_id, content, source, metadata,
               SUM(1.0 / (rrf_k + rnk)) AS score
        FROM (
            SELECT chunk_id, content, source, metadata, rnk FROM dense
            UNION ALL
            SELECT chunk_id, content, source, metadata, rnk FROM sparse
        ) u
        GROUP BY chunk_id, content, source, metadata
    )
    SELECT chunk_id, content, score::float, source, metadata
    FROM fused
    ORDER BY score DESC
    LIMIT top_k;
END;
$$;

COMMENT ON FUNCTION ai.search_hybrid(text, text, int, int, jsonb) IS
    'Hybrid retrieval. RRF fuses dense (ai.search) and BM25 (pg_textsearch <@>) rankings.';
