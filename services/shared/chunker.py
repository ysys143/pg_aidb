"""Pure-Python chunking with multiple strategies. No langchain.

Strategies:
  semantic   — embed sentences + split where consecutive distance exceeds threshold
  fixed      — character window with overlap
  recursive  — try separators in priority order (paragraph > line > sentence > space)
  paragraph  — split on blank lines, enforce min/max size

All async-friendly via the `chunk()` dispatcher. `embed_fn` only needed for semantic.
"""
import re
from typing import Awaitable, Callable, Optional

import numpy as np

# Split on:
#   - sentence terminators (.!?。!?) followed by whitespace
#   - blank lines (paragraph boundaries)
#   - just before a markdown header (so headers become their own pieces)
_SPLIT_RE = re.compile(
    r"(?<=[.!?。!?])\s+"
    r"|\n\s*\n"
    r"|(?=\n#{1,6}\s)"
)


def split_sentences(text: str) -> list[str]:
    pieces = [s.strip() for s in _SPLIT_RE.split(text) if s.strip()]
    return pieces or [text]


# ---------------------------------------------------------------------------
# Size enforcement
# ---------------------------------------------------------------------------

def _enforce_size(chunks: list[str], min_size: int, max_size: int) -> list[str]:
    """Split chunks larger than max_size; merge chunks smaller than min_size with neighbors."""
    if max_size > 0:
        split_out: list[str] = []
        for c in chunks:
            if len(c) <= max_size:
                split_out.append(c)
            else:
                split_out.extend(c[i:i + max_size] for i in range(0, len(c), max_size))
        chunks = split_out

    if min_size > 0:
        merged: list[str] = []
        buf = ""
        for c in chunks:
            if len(buf) < min_size:
                buf = (buf + " " + c).strip() if buf else c
            else:
                merged.append(buf)
                buf = c
        if buf:
            if merged and len(buf) < min_size:
                merged[-1] = merged[-1] + " " + buf
            else:
                merged.append(buf)
        chunks = merged

    return chunks


# ---------------------------------------------------------------------------
# Semantic
# ---------------------------------------------------------------------------

def _cosine(a: list[float], b: list[float]) -> float:
    av, bv = np.asarray(a), np.asarray(b)
    denom = float(np.linalg.norm(av) * np.linalg.norm(bv)) or 1.0
    return float(np.dot(av, bv) / denom)


async def semantic_chunk(
    text: str,
    embed_fn: Callable[[list[str]], Awaitable[list[list[float]]]],
    *,
    threshold_type: str = "percentile",   # percentile | stddev | interquartile
    percentile: float = 90.0,
    stddev_multiplier: float = 1.5,
    min_chunk_size: int = 100,
    max_chunk_size: int = 2000,
) -> list[str]:
    """Split where consecutive-sentence cosine distance exceeds an adaptive threshold."""
    sentences = split_sentences(text)
    if len(sentences) < 2:
        return _enforce_size(sentences, min_chunk_size, max_chunk_size)

    embeddings = await embed_fn(sentences)
    distances = np.array([
        1.0 - _cosine(embeddings[i], embeddings[i + 1])
        for i in range(len(sentences) - 1)
    ])

    if threshold_type == "percentile":
        threshold = float(np.percentile(distances, percentile))
    elif threshold_type == "stddev":
        threshold = float(distances.mean() + stddev_multiplier * distances.std())
    elif threshold_type == "interquartile":
        q1, q3 = np.percentile(distances, [25, 75])
        threshold = float(q3 + 1.5 * (q3 - q1))
    else:
        raise ValueError(f"Unknown threshold_type: {threshold_type!r}")

    chunks: list[str] = []
    current: list[str] = [sentences[0]]
    for i, d in enumerate(distances):
        if d > threshold:
            chunks.append(" ".join(current))
            current = [sentences[i + 1]]
        else:
            current.append(sentences[i + 1])
    chunks.append(" ".join(current))

    return _enforce_size([c for c in chunks if c.strip()], min_chunk_size, max_chunk_size)


# ---------------------------------------------------------------------------
# Fixed window
# ---------------------------------------------------------------------------

def fixed_chunk(
    text: str,
    *,
    chunk_size: int = 1000,
    overlap: int = 100,
) -> list[str]:
    if len(text) <= chunk_size:
        return [text]
    step = max(1, chunk_size - overlap)
    return [text[i:i + chunk_size] for i in range(0, len(text), step) if text[i:i + chunk_size]]


# ---------------------------------------------------------------------------
# Recursive (langchain-style)
# ---------------------------------------------------------------------------

_DEFAULT_SEPARATORS = ["\n\n", "\n", ". ", " "]


def recursive_chunk(
    text: str,
    *,
    chunk_size: int = 1000,
    separators: Optional[list[str]] = None,
) -> list[str]:
    """Try a priority list of separators. Falls back to fixed window if none fit."""
    seps = separators if separators is not None else _DEFAULT_SEPARATORS
    if len(text) <= chunk_size:
        return [text]

    for idx, sep in enumerate(seps):
        if sep in text:
            pieces = text.split(sep)
            chunks, current = [], ""
            for p in pieces:
                candidate = (current + sep + p) if current else p
                if len(candidate) <= chunk_size:
                    current = candidate
                else:
                    if current:
                        chunks.append(current)
                    if len(p) > chunk_size:
                        chunks.extend(recursive_chunk(p, chunk_size=chunk_size, separators=seps[idx + 1:]))
                        current = ""
                    else:
                        current = p
            if current:
                chunks.append(current)
            return chunks

    return fixed_chunk(text, chunk_size=chunk_size, overlap=0)


# ---------------------------------------------------------------------------
# Paragraph
# ---------------------------------------------------------------------------

def paragraph_chunk(
    text: str,
    *,
    min_size: int = 100,
    max_size: int = 2000,
) -> list[str]:
    paragraphs = [p.strip() for p in re.split(r"\n\s*\n", text) if p.strip()]
    return _enforce_size(paragraphs or [text], min_size, max_size)


# ---------------------------------------------------------------------------
# Parent (unchanged — fixed window for parent-child retrieval)
# ---------------------------------------------------------------------------

def parent_chunk(text: str, chunk_size: int = 2000, overlap: int = 100) -> list[str]:
    return fixed_chunk(text, chunk_size=chunk_size, overlap=overlap)


# ---------------------------------------------------------------------------
# Dispatcher
# ---------------------------------------------------------------------------

async def chunk(
    text: str,
    method: str = "semantic",
    embed_fn: Optional[Callable[[list[str]], Awaitable[list[list[float]]]]] = None,
    **kwargs,
) -> list[str]:
    """Dispatch to the named chunking strategy.

    Valid methods: semantic, fixed, recursive, paragraph.
    `embed_fn` is required for 'semantic'.
    """
    if method == "semantic":
        if embed_fn is None:
            raise ValueError("semantic chunking requires embed_fn")
        return await semantic_chunk(text, embed_fn, **kwargs)
    if method == "fixed":
        return fixed_chunk(text, **kwargs)
    if method == "recursive":
        return recursive_chunk(text, **kwargs)
    if method == "paragraph":
        return paragraph_chunk(text, **kwargs)
    raise ValueError(
        f"Unknown chunking method: {method!r}. "
        "Valid: semantic, fixed, recursive, paragraph"
    )
