# pg_aidb Performance Benchmarks

> Generated: 2026-05-22 01:49 UTC
> Stack: pg_aidb v0.1, PostgreSQL 17, pgvector 0.8, pg_textsearch, textsearch_ko (MeCab)
> Environment: local Docker (Colima/aarch64), OpenAI text-embedding-3-small

---

## 1. Search Latency (p50 / p95 / p99)

| Mode | p50 | p95 | p99 | mean |
|---|---|---|---|---|
| Dense (pgvector HNSW) | 229.2ms | 286.7ms | 297.1ms | 233.0ms |
| Hybrid (BM25 + dense + RRF) | 228.2ms | 315.6ms | 335.8ms | 235.6ms |
| MMR (fetch_k=20, λ=0.5) | 241.5ms | 266.3ms | 270.6ms | 241.4ms |

Queries: 10 varied queries. Each call embeds the query via OpenAI API and searches locally.
Hybrid overhead vs dense = network + BM25 scan. MMR overhead = numpy cosine loop over fetch_k candidates.

---

## 2. Ask Latency (prune strategy, top_k=3)

| Metric | Value |
|---|---|
| p50 | 1580.9ms |
| p95 | 1746.4ms |
| mean | 1511.5ms |

Includes: embed (OpenAI) + pgvector search + LLM generation (OpenAI). Network dominates.

---

## Notes

- All timings include OpenAI API round-trips (embed + LLM). Pure DB latency is much lower.
- pgai comparison planned (F1 v2). pgai uses vectorizer-based auto-sync vs pg_aidb's explicit ingest.
- Hybrid vs dense trade-off: BM25 adds ~-1ms overhead but improves keyword recall (NDCG +0.16 per textsearch benchmark).
