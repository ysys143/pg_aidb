"""Embedding provider abstraction.

Uses OpenAI Embeddings API. Compatible with any OpenAI-compatible endpoint
(Gemini OpenAI mode, Ollama, vLLM) via OPENAI_BASE_URL — no provider dispatch
needed for embeddings since Anthropic has no embeddings API and most providers
that DO have embeddings expose them via the OpenAI shape.

Environment variables:
  EMBED_MODEL     — embedding model name
  OPENAI_API_KEY  — API key (any OpenAI-compat provider)
  OPENAI_BASE_URL — endpoint override
"""
import os

from openai import AsyncOpenAI


def _client() -> AsyncOpenAI:
    return AsyncOpenAI(
        api_key=os.environ["OPENAI_API_KEY"],
        base_url=os.environ.get("OPENAI_BASE_URL") or None,
    )


async def embed(texts: list[str]) -> list[list[float]]:
    """Embed a batch of texts. Returns one float vector per input."""
    vectors, _ = await embed_with_usage(texts)
    return vectors


async def embed_with_usage(texts: list[str]) -> tuple[list[list[float]], dict]:
    """Same as embed() but also returns OpenAI usage metadata for cost tracking.

    Passes dimensions= when EMBED_DIMENSIONS is set, so providers that allow
    Matryoshka-style truncation (OpenAI text-embedding-3-*, Gemini embedding-*)
    return the requested width. pgvector HNSW index has a 2000-dim limit, so
    high-dim models (Gemini default 3072) MUST be truncated.
    """
    model = os.environ.get("EMBED_MODEL", "text-embedding-3-small")
    kwargs: dict = {"model": model, "input": texts}
    dims = os.environ.get("EMBED_DIMENSIONS", "")
    if dims:
        kwargs["dimensions"] = int(dims)
    resp = await _client().embeddings.create(**kwargs)
    # Some OpenAI-compatible providers (e.g. Gemini) omit the `usage` field.
    usage = {
        "model": resp.model,
        "prompt_tokens": resp.usage.prompt_tokens if resp.usage else 0,
        "total_tokens": resp.usage.total_tokens if resp.usage else 0,
    }
    return [item.embedding for item in resp.data], usage
