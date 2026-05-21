"""
rag service: similarity search and LLM-augmented generation for pg_aidb.

Endpoints called by the extension (via ai.search / ai.ask):
  POST /search  — embed query + pgvector cosine search → list of ChunkResult
  POST /ask     — search + LLM answer generation → plain text answer

Also exposes:
  POST /v1/embeddings — OpenAI-compatible embed endpoint (for ai.embed_raw)
  GET  /health
"""
import os
import logging
import time
from typing import Any

# Strip empty BASE_URL env vars — SDKs sometimes read them directly,
# and "" is not the same as unset (it causes "missing protocol" errors).
for _var in ("OPENAI_BASE_URL", "ANTHROPIC_BASE_URL"):
    if os.environ.get(_var) == "":
        del os.environ[_var]

import psycopg
from fastapi import FastAPI, HTTPException
from pydantic import BaseModel

from openai.types.chat import ChatCompletionMessageParam

from embedder import embed_with_usage as embedder_embed_with_usage
from llm import generate_with_usage as llm_generate_with_usage
from structured_log import setup_json_logging

setup_json_logging()
logger = logging.getLogger("rag")

DATABASE_URL: str = os.environ["DATABASE_URL"]

app = FastAPI(title="pg_aidb rag service")


# ---------------------------------------------------------------------------
# Request / Response models
# ---------------------------------------------------------------------------

class SearchRequest(BaseModel):
    query: str
    collection: str = "default"
    top_k: int = 5
    filter: dict[str, Any] = {}


class ChunkResult(BaseModel):
    chunk_id: str
    content: str
    similarity: float
    source: str
    metadata: dict[str, Any] = {}


class SearchResponse(BaseModel):
    results: list[ChunkResult]
    usage: dict[str, Any] = {}


class AskRequest(BaseModel):
    query: str
    collection: str = "default"
    top_k: int = 5
    filter: dict[str, Any] = {}


class AskResponse(BaseModel):
    answer: str
    usage: dict[str, Any] = {}


class EmbedRequest(BaseModel):
    input: str | list[str]
    model: str = ""


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _vec_literal(floats: list[float]) -> str:
    return "[" + ",".join(f"{x:.8f}" for x in floats) + "]"


async def vector_search(
    query_embedding: list[float],
    collection: str,
    top_k: int,
    filter: dict[str, Any] | None = None,
) -> list[ChunkResult]:
    vec = _vec_literal(query_embedding)
    import json
    filter_json = json.dumps(filter or {})
    async with await psycopg.AsyncConnection.connect(DATABASE_URL) as conn:
        rows = await (
            await conn.execute(
                """
                SELECT
                    c.id::text,
                    c.content,
                    1 - (c.embedding <=> %(vec)s::vector) AS similarity,
                    d.source,
                    c.metadata
                FROM ai.chunks c
                JOIN ai.documents d ON d.id = c.document_id
                WHERE c.collection = %(col)s
                  AND c.embedding IS NOT NULL
                  AND (%(filter)s::jsonb = '{}'::jsonb OR c.metadata @> %(filter)s::jsonb)
                ORDER BY c.embedding <=> %(vec)s::vector
                LIMIT %(k)s
                """,
                {"vec": vec, "col": collection, "k": top_k, "filter": filter_json},
            )
        ).fetchall()

    return [
        ChunkResult(
            chunk_id=row[0],
            content=row[1],
            similarity=float(row[2]),
            source=row[3],
            metadata=row[4] or {},
        )
        for row in rows
    ]


# ---------------------------------------------------------------------------
# Endpoints
# ---------------------------------------------------------------------------

@app.get("/health")
async def health():
    return {"status": "ok", "service": "rag"}


@app.post("/v1/embeddings")
async def embeddings(req: EmbedRequest):
    """OpenAI-compatible endpoint so ai.embed_raw can route here."""
    inputs = req.input if isinstance(req.input, list) else [req.input]
    vectors, usage = await embedder_embed_with_usage(inputs)
    embed_model = os.environ.get("EMBED_MODEL", "text-embedding-3-small")
    return {
        "object": "list",
        "model": embed_model,
        "data": [
            {"object": "embedding", "index": i, "embedding": vec}
            for i, vec in enumerate(vectors)
        ],
        "usage": usage,
    }


@app.post("/search", response_model=SearchResponse)
async def search(req: SearchRequest) -> SearchResponse:
    """Retrieval — returns ranked chunks plus embedding usage."""
    t0 = time.time()
    try:
        vecs, usage = await embedder_embed_with_usage([req.query])
        results = await vector_search(vecs[0], req.collection, req.top_k, req.filter)
    except Exception as exc:
        logger.exception("search failed", extra={"op": "search", "collection": req.collection})
        raise HTTPException(status_code=500, detail=str(exc))
    logger.info("search done", extra={
        "op": "search",
        "collection": req.collection,
        "top_k": req.top_k,
        "n_results": len(results),
        "duration_ms": int((time.time() - t0) * 1000),
        "usage": usage,
    })
    return SearchResponse(results=results, usage=usage)


@app.post("/ask", response_model=AskResponse)
async def ask(req: AskRequest) -> AskResponse:
    """Retrieval + LLM generation. Returns answer and combined embed+LLM usage."""
    t0 = time.time()
    try:
        sr = await search(SearchRequest(
            query=req.query, collection=req.collection, top_k=req.top_k, filter=req.filter
        ))
        chunks = sr.results
        embed_usage = sr.usage
    except HTTPException:
        raise
    except Exception as exc:
        logger.exception("ask search phase failed", extra={"op": "ask"})
        raise HTTPException(status_code=500, detail=str(exc))

    if not chunks:
        return AskResponse(
            answer="No relevant context found in the collection.",
            usage={"embed": embed_usage},
        )

    context = "\n\n---\n\n".join(f"[Source: {c.source}]\n{c.content}" for c in chunks)
    messages: list[ChatCompletionMessageParam] = [
        {
            "role": "system",
            "content": (
                "You are a precise assistant. Answer the user's question "
                "using only the provided context. "
                "If the answer is not in the context, say so clearly."
            ),
        },
        {
            "role": "user",
            "content": f"Context:\n{context}\n\nQuestion: {req.query}",
        },
    ]
    try:
        answer, gen_usage = await llm_generate_with_usage(messages)
    except Exception as exc:
        logger.exception("LLM call failed", extra={"op": "ask"})
        raise HTTPException(status_code=500, detail=f"LLM error: {exc}")

    combined = {
        "embed": embed_usage,
        "generate": gen_usage,
        "total_tokens": embed_usage.get("total_tokens", 0) + gen_usage.get("total_tokens", 0),
    }
    logger.info("ask done", extra={
        "op": "ask",
        "collection": req.collection,
        "n_chunks": len(chunks),
        "answer_chars": len(answer),
        "duration_ms": int((time.time() - t0) * 1000),
        "usage": combined,
    })
    return AskResponse(answer=answer, usage=combined)
