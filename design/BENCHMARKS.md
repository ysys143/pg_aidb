# pg_aidb Performance Benchmarks

> Generated: 2026-05-22 01:44 UTC
> Stack: pg_aidb v0.1, PostgreSQL 17, pgvector 0.8, pg_textsearch, textsearch_ko (MeCab)
> Environment: local Docker (Colima/aarch64), OpenAI text-embedding-3-small

---

## 1. Ingest Throughput

| Metric | Value |
|---|---|
| Documents | 20 |
| Total time | 1.05s |
| **Throughput** | **1141.3 docs/min** |

Pipeline: ai.ingest (inline text) → pipeline-worker (async embed + chunk + pgvector store)

---

## 2. Search Latency (p50 / p95 / p99)

| Mode | p50 | p95 | p99 | mean |
|---|---|---|---|---|
| Dense (pgvector HNSW) | 227.8ms | 284.8ms | 296.6ms | 231.0ms |
| Hybrid (BM25 + dense + RRF) | 226.4ms | 394.3ms | 439.9ms | 239.8ms |
| MMR (fetch_k=20, λ=0.5) | 229.2ms | 286.9ms | 297.5ms | 233.9ms |

Queries: 10 varied queries. Each call embeds the query via OpenAI API and searches locally.
Hybrid overhead vs dense = network + BM25 scan. MMR overhead = numpy cosine loop over fetch_k candidates.

---

## 3. Ask Latency (prune strategy, top_k=3)

| Metric | Value |
|---|---|
| p50 | 1294.8ms |
| p95 | 2176.1ms |
| mean | 1437.5ms |

Includes: embed (OpenAI) + pgvector search + LLM generation (OpenAI). Network dominates.

---

## Notes

- All timings include OpenAI API round-trips (embed + LLM). Pure DB latency is much lower.
- pgai comparison planned (F1 v2). pgai uses vectorizer-based auto-sync vs pg_aidb's explicit ingest.
- Hybrid vs dense: p50 差 negligible (~noise), p95 +109ms overhead (BM25 scan + RRF sort). Keyword recall improves NDCG +0.16 per textsearch benchmark — worthwhile for Korean keyword queries.
- Local embed model (e.g. Ollama bge-m3) would drop search p50 to ~10-30ms (no network round-trip).
