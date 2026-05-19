"""
pipeline-worker: document ingestion service for pg_aidb.

Flow:
  1. On startup: run sql/schema.sql to create ai.documents + ai.chunks tables.
  2. LISTEN on 'aidb_pipeline' PostgreSQL channel (ADR-001 Outbox pattern).
  3. On NOTIFY: read payload from ai._outbox, parse → chunk → embed → store.
  4. Update ai.results status when done.
"""
import asyncio
import json
import logging
import os
from contextlib import asynccontextmanager
from pathlib import Path

# Strip empty BASE_URL env vars — SDKs sometimes read them directly,
# and "" is not the same as unset (it causes "missing protocol" errors).
for _var in ("OPENAI_BASE_URL", "ANTHROPIC_BASE_URL"):
    if os.environ.get(_var) == "":
        del os.environ[_var]

import psycopg
from fastapi import FastAPI

from structured_log import setup_json_logging
setup_json_logging()
logger = logging.getLogger("pipeline-worker")

DATABASE_URL: str = os.environ["DATABASE_URL"]


# ---------------------------------------------------------------------------
# Schema initialisation
# ---------------------------------------------------------------------------

async def init_schema() -> None:
    """Run schema.sql with EMBED_DIMENSIONS substituted for the vector column width.

    Verifies that an existing ai.chunks table matches the configured dimension —
    silent mismatch (CREATE IF NOT EXISTS skips) would cause INSERTs to fail later.
    """
    import re
    from typing import LiteralString, cast
    from psycopg.sql import SQL

    embed_dims = int(os.environ.get("EMBED_DIMENSIONS", "1536"))
    template = (Path(__file__).parent.parent / "sql" / "schema.sql").read_text()
    # Single sentinel — schema.sql uses literal 'vector(1536)' which we rewrite.
    schema = template.replace("vector(1536)", f"vector({embed_dims})")

    async with await psycopg.AsyncConnection.connect(DATABASE_URL) as conn:
        # If ai.chunks already exists with different dim, fail loudly.
        row = await (
            await conn.execute(
                "SELECT format_type(atttypid, atttypmod) FROM pg_attribute "
                "WHERE attrelid = to_regclass('ai.chunks') AND attname = 'embedding'"
            )
        ).fetchone()
        if row is not None:
            m = re.match(r"vector\((\d+)\)", row[0])
            current_dim = int(m.group(1)) if m else None
            if current_dim is not None and current_dim != embed_dims:
                raise RuntimeError(
                    f"ai.chunks.embedding is vector({current_dim}) but EMBED_DIMENSIONS={embed_dims}. "
                    "Drop the table (or rebuild from scratch) before changing the embedding model."
                )

        await conn.execute(SQL(cast(LiteralString, schema)))
        await conn.commit()
    logger.info("Schema initialised", extra={"embed_dims": embed_dims})


# ---------------------------------------------------------------------------
# Document parsing
# ---------------------------------------------------------------------------

def parse_document(source: str) -> str:
    """Extract text from a document file.

    Uses opendataloader-pdf directly (no langchain) for binary formats.
    convert() writes output to a directory; we read the markdown file back.
    """
    ext = Path(source).suffix.lower()
    if ext not in (".pdf", ".docx", ".hwp", ".pptx", ".xlsx"):
        return Path(source).read_text(encoding="utf-8", errors="ignore")

    import tempfile

    import opendataloader_pdf

    with tempfile.TemporaryDirectory() as out_dir:
        opendataloader_pdf.convert(
            input_path=[source],
            output_dir=out_dir,
            format="markdown",
        )
        md_files = list(Path(out_dir).rglob("*.md"))
        if not md_files:
            raise RuntimeError(f"opendataloader produced no markdown for {source}")
        return "\n\n".join(f.read_text(encoding="utf-8") for f in md_files)


# ---------------------------------------------------------------------------
# Chunking — delegate to chunker.py (no langchain)
# ---------------------------------------------------------------------------

async def chunk_text(text: str, config: dict) -> list[str]:
    """Dispatch to the chunking strategy named in config.

    config.chunking may be either:
      - a dict: {"method": "semantic", "percentile": 90, ...}
      - a string: "semantic"  (shorthand, no extra params)
      - absent:  defaults to semantic with library defaults
    """
    from chunker import chunk
    from embedder import embed

    cfg = config.get("chunking", {})
    if isinstance(cfg, str):
        cfg = {"method": cfg}
    method = cfg.get("method", "semantic")
    kwargs = {k: v for k, v in cfg.items() if k != "method"}
    return await chunk(text, method=method, embed_fn=embed, **kwargs)


def parent_chunk_text(text: str, chunk_size: int = 2000) -> list[str]:
    from chunker import parent_chunk
    return parent_chunk(text, chunk_size=chunk_size)


# ---------------------------------------------------------------------------
# Embedding
# ---------------------------------------------------------------------------

async def embed_batch(texts: list[str]) -> list[list[float]]:
    """Batch-embed texts via the shared embedder abstraction."""
    from embedder import embed
    return await embed(texts)


def _vec_literal(floats: list[float]) -> str:
    """Convert float list to pgvector literal string: '[0.1,0.2,...]'"""
    return "[" + ",".join(f"{x:.8f}" for x in floats) + "]"


# ---------------------------------------------------------------------------
# Storage
# ---------------------------------------------------------------------------

async def store_chunks(
    source: str,
    content: str,
    collection: str,
    child_chunks: list[str],
    parent_chunks: list[str],
    request_id: str,
) -> None:
    async with await psycopg.AsyncConnection.connect(DATABASE_URL) as conn:
        # Insert document record
        row = await (
            await conn.execute(
                "INSERT INTO ai.documents(collection, source, content) "
                "VALUES (%s, %s, %s) RETURNING id",
                (collection, source, content),
            )
        ).fetchone()
        assert row is not None, "INSERT INTO ai.documents RETURNING returned None"
        doc_id = row[0]

        # Embed + insert parent chunks (if any)
        parent_ids: dict[int, object] = {}
        if parent_chunks:
            parent_embeddings = await embed_batch(parent_chunks)
            for i, (text, emb) in enumerate(zip(parent_chunks, parent_embeddings)):
                pid_row = await (
                    await conn.execute(
                        "INSERT INTO ai.chunks"
                        "(document_id, collection, content, chunk_index, embedding, metadata) "
                        "VALUES (%s, %s, %s, %s, %s::vector, %s::jsonb) RETURNING id",
                        (doc_id, collection, text, i, _vec_literal(emb),
                         json.dumps({"type": "parent"})),
                    )
                ).fetchone()
                assert pid_row is not None, "INSERT INTO ai.chunks RETURNING returned None"
                parent_ids[i] = pid_row[0]

        # Embed + insert child chunks
        child_embeddings = await embed_batch(child_chunks)
        n_parents = len(parent_ids)
        for i, (text, emb) in enumerate(zip(child_chunks, child_embeddings)):
            parent_id = None
            if n_parents:
                # Map child index → nearest parent by proportion
                p_idx = min(int(i * n_parents / len(child_chunks)), n_parents - 1)
                parent_id = parent_ids.get(p_idx)
            await conn.execute(
                "INSERT INTO ai.chunks"
                "(document_id, parent_chunk_id, collection, content, "
                " chunk_index, embedding, metadata) "
                "VALUES (%s, %s, %s, %s, %s, %s::vector, %s::jsonb)",
                (doc_id, parent_id, collection, text, i,
                 _vec_literal(emb), json.dumps({"type": "child"})),
            )

        # Mark request as done in ai.results
        await conn.execute(
            "UPDATE ai.results SET status='done', finished_at=now() "
            "WHERE request_id = %s::uuid",
            (request_id,),
        )
        await conn.commit()


# ---------------------------------------------------------------------------
# Ingest pipeline
# ---------------------------------------------------------------------------

RAG_URL: str = os.environ.get("RAG_URL", "http://rag:8002")


async def _claim_outbox(request_id: str) -> tuple[str, dict] | None:
    """Claim one outbox row for processing. Returns (event_type, payload) or None."""
    async with await psycopg.AsyncConnection.connect(DATABASE_URL) as conn:
        row = await (
            await conn.execute(
                "UPDATE ai._outbox SET taken_at = now() "
                "WHERE id = ("
                "  SELECT id FROM ai._outbox "
                "  WHERE payload->>'request_id' = %s AND taken_at IS NULL "
                "  ORDER BY created_at LIMIT 1 "
                "  FOR UPDATE SKIP LOCKED"
                ") RETURNING event_type, payload",
                (request_id,),
            )
        ).fetchone()
        await conn.commit()
    if row is None:
        return None
    return row[0], row[1]


async def _mark_error(request_id: str, exc: Exception) -> None:
    async with await psycopg.AsyncConnection.connect(DATABASE_URL) as conn:
        await conn.execute(
            "UPDATE ai.results SET status='error', finished_at=now(), error_msg=%s "
            "WHERE request_id = %s::uuid",
            (str(exc), request_id),
        )
        await conn.commit()


async def _process_ingest(request_id: str, payload: dict) -> None:
    source: str = payload["source"]
    content: str = payload.get("content", "")
    collection: str = payload.get("collection", "default")
    config: dict = payload.get("config", {})

    logger.info("Processing ingest source=%s collection=%s", source, collection)
    if not content:
        content = parse_document(source)

    child_chunks = await chunk_text(content, config)
    parent_chunks: list[str] = []
    if config.get("parent_child", False):
        parent_chunks = parent_chunk_text(content, config.get("parent_chunk_size", 2000))

    await store_chunks(source, content, collection, child_chunks, parent_chunks, request_id)
    logger.info("Ingested %d child chunks for request_id=%s", len(child_chunks), request_id)


async def _process_search(request_id: str, payload: dict) -> None:
    """Call rag /search; persist results + usage to ai.results.data."""
    import httpx
    body = {"query": payload["query"], "collection": payload["collection"], "top_k": payload["top_k"]}
    async with httpx.AsyncClient(timeout=60) as client:
        resp = await client.post(f"{RAG_URL}/search", json=body)
        resp.raise_for_status()
        wrapped = resp.json()  # {"results": [...], "usage": {...}}
    results = wrapped.get("results", [])
    usage   = wrapped.get("usage", {})

    async with await psycopg.AsyncConnection.connect(DATABASE_URL) as conn:
        await conn.execute(
            "UPDATE ai.results SET status='done', finished_at=now(), "
            "  data = data || jsonb_build_object('results', %s::jsonb, 'usage', %s::jsonb) "
            "WHERE request_id = %s::uuid",
            (json.dumps(results), json.dumps(usage), request_id),
        )
        await conn.commit()
    logger.info("search done", extra={
        "request_id": request_id, "op": "search",
        "n_results": len(results), "usage": usage,
    })


async def _process_ask(request_id: str, payload: dict) -> None:
    """Call rag /ask; persist answer + usage to ai.results.data."""
    import httpx
    body = {"query": payload["query"], "collection": payload["collection"], "top_k": payload["top_k"]}
    async with httpx.AsyncClient(timeout=120) as client:
        resp = await client.post(f"{RAG_URL}/ask", json=body)
        resp.raise_for_status()
        wrapped = resp.json()  # {"answer": "...", "usage": {...}}
    answer = wrapped.get("answer", "")
    usage  = wrapped.get("usage", {})

    async with await psycopg.AsyncConnection.connect(DATABASE_URL) as conn:
        await conn.execute(
            "UPDATE ai.results SET status='done', finished_at=now(), "
            "  data = data || jsonb_build_object('answer', %s::text, 'usage', %s::jsonb) "
            "WHERE request_id = %s::uuid",
            (answer, json.dumps(usage), request_id),
        )
        await conn.commit()
    logger.info("ask done", extra={
        "request_id": request_id, "op": "ask",
        "answer_chars": len(answer), "usage": usage,
    })


async def process_event(request_id: str) -> None:
    """Dispatch outbox event to the appropriate handler by event_type."""
    claim = await _claim_outbox(request_id)
    if claim is None:
        logger.warning("No unclaimed outbox row for request_id=%s", request_id)
        return
    event_type, payload = claim

    handler = {
        "ingest": _process_ingest,
        "search": _process_search,
        "ask": _process_ask,
    }.get(event_type)
    if handler is None:
        logger.warning("Unknown event_type=%s request_id=%s", event_type, request_id)
        return

    try:
        await handler(request_id, payload)
    except Exception as exc:
        logger.exception("%s failed request_id=%s", event_type, request_id)
        await _mark_error(request_id, exc)


# ---------------------------------------------------------------------------
# LISTEN loop
# ---------------------------------------------------------------------------

async def listen_loop() -> None:
    """Persistent LISTEN loop; reconnects automatically on failure."""
    while True:
        try:
            async with await psycopg.AsyncConnection.connect(
                DATABASE_URL, autocommit=True
            ) as conn:
                await conn.execute("LISTEN aidb_pipeline")
                logger.info("Listening on aidb_pipeline channel")
                async for notify in conn.notifies():
                    # payload = request_id uuid string; event_type comes from ai._outbox row
                    asyncio.create_task(process_event(notify.payload))
        except asyncio.CancelledError:
            break
        except Exception:
            logger.exception("Listen loop error; reconnecting in 5 s")
            await asyncio.sleep(5)


# ---------------------------------------------------------------------------
# FastAPI app
# ---------------------------------------------------------------------------

@asynccontextmanager
async def lifespan(app: FastAPI):
    await init_schema()
    task = asyncio.create_task(listen_loop())
    try:
        yield
    finally:
        task.cancel()
        try:
            await task
        except asyncio.CancelledError:
            pass


app = FastAPI(title="pg_aidb pipeline-worker", lifespan=lifespan)


@app.get("/health")
async def health():
    return {"status": "ok", "service": "pipeline-worker"}
