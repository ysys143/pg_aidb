"""Regenerate sample.pdf — multi-topic excerpts from foundational database papers
and PostgreSQL history. Used by the opendataloader E2E test.

Sources (brief excerpts for testing; verbatim reproduction is fair use):
  - Codd, E.F. (1970). "A Relational Model of Data for Large Shared Data Banks."
    Communications of the ACM, 13(6), 377-387.
  - Stonebraker, M., Rowe, L.A. (1986). "The Design of POSTGRES."
    SIGMOD Record, 15(2), 340-355.
  - PostgreSQL Project (BSD-licensed docs). "A Brief History of PostgreSQL."

Run:
  uv pip install reportlab
  cd tests/fixtures && python generate.py
"""
from pathlib import Path

from reportlab.lib.pagesizes import letter
from reportlab.lib.styles import getSampleStyleSheet
from reportlab.platypus import Paragraph, SimpleDocTemplate, Spacer

OUT = Path(__file__).parent / "sample.pdf"

SECTIONS = [
    (
        "Codd (1970): The Relational Model",
        [
            "Future users of large data banks must be protected from having to know "
            "how the data is organized in the machine — the internal representation.",
            "A prompting service which supplies such information is not a satisfactory "
            "solution. Activities of users at terminals and most application programs "
            "should remain unaffected when the internal representation of data is changed.",
            "Changes in data representation will often be needed as a result of changes "
            "in query, update, and report traffic and natural growth in the types of "
            "stored information.",
            "Existing noninferential, formatted data systems provide users with tree-structured "
            "files or slightly more general network models of the data.",
            "In Section 1, inadequacies of these models are discussed. A model based on n-ary "
            "relations, a normal form for database relations, and the concept of a universal "
            "data sublanguage are introduced.",
        ],
    ),
    (
        "Stonebraker & Rowe (1986): The Design of POSTGRES",
        [
            "This paper presents the preliminary design of a new database management system, "
            "called POSTGRES, that is the successor to the INGRES relational database system.",
            "The main design goals of the new system are to provide better support for complex "
            "objects, to provide user extensibility for data types, operators and access methods, "
            "and to provide facilities for active databases (i.e., alerters and triggers) and "
            "inferencing including forward- and backward-chaining.",
            "POSTGRES makes as few changes as possible to the relational model. As a result, "
            "the query language QUEL is only slightly altered to become POSTQUEL.",
            "Many of the most important changes in POSTGRES occur below the query language "
            "interface, in the implementation of data types, access methods, and the "
            "transaction system.",
            "Furthermore, POSTGRES is designed to run on a shared memory multiprocessor and "
            "include a novel storage system that uses an append-only design with garbage "
            "collection of obsolete tuples.",
        ],
    ),
    (
        "PostgreSQL: From Berkeley to Global Open Source",
        [
            "The object-relational database management system now known as PostgreSQL is "
            "derived from the POSTGRES package written at the University of California at "
            "Berkeley.",
            "With over two decades of development behind it, PostgreSQL is now the most "
            "advanced open-source database available anywhere, supporting a large part of "
            "the SQL standard and offering many modern features.",
            "Some of these features are complex queries, foreign keys, triggers, updatable "
            "views, transactional integrity, and multiversion concurrency control.",
            "Also, PostgreSQL can be extended by the user in many ways, for example by adding "
            "new data types, functions, operators, aggregate functions, index methods and "
            "procedural languages.",
            "And because of the liberal license, PostgreSQL can be used, modified, and "
            "distributed by anyone free of charge for any purpose, be it private, commercial, "
            "or academic.",
        ],
    ),
    (
        "Modern Era: Vector Search and AI Workloads",
        [
            "The pgvector extension adds vector data types and similarity-search operators "
            "to PostgreSQL, enabling semantic search and retrieval-augmented generation "
            "directly inside the database.",
            "Vector embeddings are produced by language models such as the OpenAI "
            "text-embedding-3-small model, which returns 1536-dimensional float vectors "
            "for arbitrary input text.",
            "Approximate-nearest-neighbor indexes (HNSW and IVFFlat) allow sub-linear "
            "lookup over millions of vectors with controllable recall.",
            "Combining traditional relational queries with vector similarity makes PostgreSQL "
            "a unified store for both structured business data and unstructured AI features.",
            "This is the foundation that pg_aidb extends with managed pipelines, model registry, "
            "and the dual-mode synchronous/asynchronous API surface.",
        ],
    ),
]


def main() -> None:
    doc = SimpleDocTemplate(str(OUT), pagesize=letter, title="DBMS History Excerpts")
    styles = getSampleStyleSheet()
    story = []
    for title, paragraphs in SECTIONS:
        story.append(Paragraph(title, styles["Heading1"]))
        story.append(Spacer(1, 6))
        for p in paragraphs:
            story.append(Paragraph(p, styles["BodyText"]))
            story.append(Spacer(1, 4))
        story.append(Spacer(1, 12))
    doc.build(story)
    n_sentences = sum(len(paras) for _, paras in SECTIONS)
    print(f"wrote {OUT} ({OUT.stat().st_size} bytes, {n_sentences} sentences across {len(SECTIONS)} topics)")


if __name__ == "__main__":
    main()
