"""
pg_aidb performance benchmark.

Measures:
  1. Ingest throughput  — docs/min (via ai.ingest + wait for pipeline-worker)
  2. Search latency     — p50/p95/p99 for dense / hybrid / MMR
  3. Ask latency        — p50/p95/p99 (prune strategy, no LLM in search path)

Usage:
  python benchmarks/run.py [--host localhost] [--port 5432] [--n-docs 20] [--n-queries 30]

Output:
  design/BENCHMARKS.md  (created/overwritten)
  benchmarks/results.json  (raw numbers)
"""
import argparse
import json
import statistics
import time
from datetime import datetime
from pathlib import Path

import psycopg

# ---------------------------------------------------------------------------
# Sample documents (varied topics for meaningful retrieval)
# ---------------------------------------------------------------------------

DOCS = [
    ("PostgreSQL is an open-source relational database with ACID compliance and extensibility via extensions.", "db-1"),
    ("pgvector adds vector similarity search to PostgreSQL using HNSW and IVFFlat index types.", "db-2"),
    ("pg_textsearch provides BM25 ranking and a bm25 access method as a PostgreSQL extension.", "db-3"),
    ("textsearch_ko integrates MeCab morphological analysis into PostgreSQL tsvector.", "db-4"),
    ("pgrx allows building PostgreSQL extensions in Rust with safe SPI bindings.", "db-5"),
    ("Python is a general-purpose programming language popular in data science and machine learning.", "lang-1"),
    ("JavaScript runs in browsers and on servers via Node.js, enabling full-stack web development.", "lang-2"),
    ("Rust provides memory safety without a garbage collector through its ownership system.", "lang-3"),
    ("Go is designed for simplicity and high-performance concurrent programming.", "lang-4"),
    ("TypeScript adds static typing to JavaScript, improving large codebase maintainability.", "lang-5"),
    ("RAG (Retrieval-Augmented Generation) grounds LLM answers in retrieved documents.", "rag-1"),
    ("Embeddings are dense vector representations that capture semantic meaning of text.", "rag-2"),
    ("RRF (Reciprocal Rank Fusion) combines rankings from multiple retrieval systems.", "rag-3"),
    ("MMR (Maximal Marginal Relevance) balances relevance and diversity in result sets.", "rag-4"),
    ("Chunking splits documents into smaller pieces to fit LLM context windows.", "rag-5"),
    ("PostgreSQL HNSW index provides approximate nearest-neighbour search in O(log n).", "vec-1"),
    ("IVFFlat partitions vector space into lists for faster approximate search.", "vec-2"),
    ("Cosine similarity measures the angle between two vectors, ignoring magnitude.", "vec-3"),
    ("BM25 is a probabilistic ranking function extending TF-IDF with document length normalisation.", "bm25-1"),
    ("Inverted index maps terms to documents, enabling efficient full-text keyword search.", "bm25-2"),
]

QUERIES = [
    "What is PostgreSQL?",
    "How does vector similarity search work?",
    "What is BM25 ranking?",
    "Explain RAG retrieval augmented generation",
    "What is HNSW index?",
    "How does RRF combine search results?",
    "What is pgrx?",
    "Explain MMR diversity in search",
    "What is MeCab morphological analysis?",
    "How does cosine similarity work?",
]


# ---------------------------------------------------------------------------
# DB helpers
# ---------------------------------------------------------------------------

def connect(host: str, port: int) -> psycopg.Connection:
    return psycopg.connect(
        f"postgresql://postgres:dev@{host}:{port}/aidb",
        autocommit=True,
    )


def setup(conn: psycopg.Connection, pipeline: str = "bench") -> None:
    conn.execute("""
        INSERT INTO ai.endpoints(name, service, base_url)
        VALUES ('rag-svc', 'rag', 'http://rag:8002')
        ON CONFLICT (name) DO UPDATE SET base_url = EXCLUDED.base_url
    """)
    conn.execute("""
        INSERT INTO ai.models(name, model_type, provider, endpoint_id)
        SELECT 'default', 'embedding', 'openai', id FROM ai.endpoints WHERE name = 'rag-svc'
        ON CONFLICT (name) DO NOTHING
    """)
    conn.execute(
        f"SELECT ai.create_pipeline('{pipeline}', '{pipeline}-col', 'default', 'default-llm', '{{}}')"  # type: ignore[arg-type]
    )


def cleanup(conn: psycopg.Connection, pipeline: str = "bench") -> None:
    conn.execute(f"DELETE FROM ai.documents WHERE collection = '{pipeline}-col'")  # type: ignore[arg-type]
    conn.execute(f"DELETE FROM ai.pipelines WHERE name = '{pipeline}'")  # type: ignore[arg-type]
    conn.execute(f"DELETE FROM ai.results WHERE data->>'source' LIKE 'bench-%'")  # type: ignore[arg-type]  # noqa: E501


# ---------------------------------------------------------------------------
# Benchmark 1: ingest throughput
# ---------------------------------------------------------------------------

def bench_ingest(conn: psycopg.Connection, docs: list, pipeline: str) -> dict:
    print(f"\n[1/3] Ingest throughput ({len(docs)} docs) ...")
    t0 = time.perf_counter()
    for content, name in docs:
        conn.execute(
            "SELECT ai.ingest(%s, %s, %s)",
            (f"bench-{name}.txt", content, pipeline),
        )

    # Wait for all to finish (max 3 minutes)
    sources = tuple(f"bench-{n}.txt" for _, n in docs)
    deadline = time.time() + 180
    while time.time() < deadline:
        row = conn.execute(
            "SELECT COUNT(*) FROM ai.results "
            "WHERE data->>'op' = 'ingest' AND data->>'source' = ANY(%s) AND status = 'done'",
            (list(sources),),
        ).fetchone()
        if row and row[0] >= len(docs):
            break
        time.sleep(1)

    elapsed = time.perf_counter() - t0
    total_chars = sum(len(content) for content, _ in docs)
    # ~4 chars per token (rough English estimate); use for relative comparison only
    est_tokens = total_chars / 4
    chars_per_min = total_chars / (elapsed / 60)
    tokens_per_min = est_tokens / (elapsed / 60)
    print(f"  Elapsed: {elapsed:.1f}s  {chars_per_min:.0f} chars/min  ~{tokens_per_min:.0f} est-tokens/min")
    return {
        "n_docs": len(docs),
        "total_chars": total_chars,
        "est_tokens": round(est_tokens),
        "elapsed_s": round(elapsed, 2),
        "chars_per_min": round(chars_per_min),
        "est_tokens_per_min": round(tokens_per_min),
    }


# ---------------------------------------------------------------------------
# Benchmark 2: search latency
# ---------------------------------------------------------------------------

def bench_search(conn: psycopg.Connection, queries: list, pipeline: str) -> dict:
    print(f"\n[2/3] Search latency ({len(queries)} queries × 3 modes) ...")
    # Note: ingest is step 1 (data prep) but throughput not reported — docs/min
    # is meaningless without controlling for doc size. Use chars/min or tokens/min.
    results: dict = {"dense": [], "hybrid": [], "mmr": []}

    for q in queries:
        # Dense
        t0 = time.perf_counter()
        conn.execute("SELECT * FROM ai.search(%s, %s, 5)", (q, pipeline)).fetchall()
        results["dense"].append(time.perf_counter() - t0)

        # Hybrid
        t0 = time.perf_counter()
        conn.execute("SELECT * FROM ai.search_hybrid(%s, %s, 5)", (q, pipeline)).fetchall()
        results["hybrid"].append(time.perf_counter() - t0)

        # MMR
        t0 = time.perf_counter()
        conn.execute("SELECT * FROM ai.search_mmr(%s, %s, 5)", (q, pipeline)).fetchall()
        results["mmr"].append(time.perf_counter() - t0)

    stats = {}
    for mode, times in results.items():
        ms = [t * 1000 for t in times]
        s = statistics.quantiles(ms, n=100)
        stats[mode] = {
            "p50_ms": round(s[49], 1),
            "p95_ms": round(s[94], 1),
            "p99_ms": round(s[98], 1),
            "mean_ms": round(statistics.mean(ms), 1),
        }
        print(f"  {mode:8s}: p50={stats[mode]['p50_ms']}ms  p95={stats[mode]['p95_ms']}ms  p99={stats[mode]['p99_ms']}ms")
    return stats


# ---------------------------------------------------------------------------
# Benchmark 3: ask latency (LLM included)
# ---------------------------------------------------------------------------

def bench_ask(conn: psycopg.Connection, queries: list, pipeline: str, n: int = 5) -> dict:
    print(f"\n[3/3] Ask latency ({n} queries, prune strategy) ...")
    subset = queries[:n]
    times = []
    for q in subset:
        t0 = time.perf_counter()
        conn.execute("SELECT ai.ask(%s, %s, 3, 2000, 'prune')", (q, pipeline)).fetchone()
        times.append(time.perf_counter() - t0)
    ms = [t * 1000 for t in times]
    s = statistics.quantiles(ms, n=100) if len(ms) >= 2 else ms
    result = {
        "p50_ms": round(s[49] if len(ms) >= 2 else ms[0], 1),
        "p95_ms": round(s[94] if len(ms) >= 2 else ms[0], 1),
        "mean_ms": round(statistics.mean(ms), 1),
    }
    print(f"  ask:     p50={result['p50_ms']}ms  p95={result['p95_ms']}ms")
    return result


# ---------------------------------------------------------------------------
# Report generation
# ---------------------------------------------------------------------------

def write_markdown(results: dict, out_path: Path) -> None:
    now = datetime.now().strftime("%Y-%m-%d %H:%M UTC")
    search = results["search"]
    ask = results["ask"]

    md = f"""# pg_aidb Performance Benchmarks

> Generated: {now}
> Stack: pg_aidb v0.1, PostgreSQL 17, pgvector 0.8, pg_textsearch, textsearch_ko (MeCab)
> Environment: local Docker (Colima/aarch64), OpenAI text-embedding-3-small

---

## 1. Search Latency (p50 / p95 / p99)

| Mode | p50 | p95 | p99 | mean |
|---|---|---|---|---|
| Dense (pgvector HNSW) | {search['dense']['p50_ms']}ms | {search['dense']['p95_ms']}ms | {search['dense']['p99_ms']}ms | {search['dense']['mean_ms']}ms |
| Hybrid (BM25 + dense + RRF) | {search['hybrid']['p50_ms']}ms | {search['hybrid']['p95_ms']}ms | {search['hybrid']['p99_ms']}ms | {search['hybrid']['mean_ms']}ms |
| MMR (fetch_k=20, λ=0.5) | {search['mmr']['p50_ms']}ms | {search['mmr']['p95_ms']}ms | {search['mmr']['p99_ms']}ms | {search['mmr']['mean_ms']}ms |

Queries: {len(QUERIES)} varied queries. Each call embeds the query via OpenAI API and searches locally.
Hybrid overhead vs dense = network + BM25 scan. MMR overhead = numpy cosine loop over fetch_k candidates.

---

## 2. Ask Latency (prune strategy, top_k=3)

| Metric | Value |
|---|---|
| p50 | {ask['p50_ms']}ms |
| p95 | {ask['p95_ms']}ms |
| mean | {ask['mean_ms']}ms |

Includes: embed (OpenAI) + pgvector search + LLM generation (OpenAI). Network dominates.

---

## Notes

- All timings include OpenAI API round-trips (embed + LLM). Pure DB latency is much lower.
- pgai comparison planned (F1 v2). pgai uses vectorizer-based auto-sync vs pg_aidb's explicit ingest.
- Hybrid vs dense trade-off: BM25 adds ~Xms overhead but improves keyword recall (NDCG +0.16 per textsearch benchmark).
"""
    # Fill hybrid overhead in notes
    overhead = search["hybrid"]["p50_ms"] - search["dense"]["p50_ms"]
    md = md.replace("~Xms", f"~{overhead:.0f}ms")

    out_path.write_text(md)
    print(f"\nReport written to {out_path}")


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="localhost")
    parser.add_argument("--port", type=int, default=5432)
    parser.add_argument("--n-docs", type=int, default=len(DOCS))
    parser.add_argument("--n-queries", type=int, default=len(QUERIES))
    args = parser.parse_args()

    docs = DOCS[: args.n_docs]
    queries = QUERIES[: args.n_queries]
    pipeline = "bench"

    print(f"pg_aidb benchmark — {args.n_docs} docs, {args.n_queries} queries")
    print(f"Connecting to {args.host}:{args.port}...")

    with connect(args.host, args.port) as conn:
        cleanup(conn, pipeline)  # clean any previous run
        setup(conn, pipeline)

        ingest_stats = bench_ingest(conn, docs, pipeline)
        search_stats = bench_search(conn, queries, pipeline)
        ask_stats = bench_ask(conn, queries, pipeline)

        cleanup(conn, pipeline)

    results = {
        "timestamp": datetime.now().isoformat(),
        "config": vars(args),
        "ingest": ingest_stats,
        "search": search_stats,
        "ask": ask_stats,
    }

    out_dir = Path(__file__).parent
    (out_dir / "results.json").write_text(json.dumps(results, indent=2))

    write_markdown(results, Path(__file__).parent.parent / "design" / "BENCHMARKS.md")
    print("\nDone.")


if __name__ == "__main__":
    main()
