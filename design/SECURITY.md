# pg_aidb Security Model

## Threat model

This extension exists to bridge PostgreSQL queries to external AI services (OpenAI,
mocks, etc.). The main assets to protect are:

| Asset | Where it lives | How it's protected |
|---|---|---|
| API keys (OpenAI etc.) | Container env vars — `OPENAI_API_KEY` | Never stored in DB. Only env var **names** are stored in `ai.endpoints.api_key_env`. The SECURITY DEFINER functions call `std::env::var()` to fetch the value at use time. |
| User queries | `ai.results.data->>'query'` | Owner-only by default (PostgreSQL table default). Admins can restrict further. |
| Ingested documents | `ai.documents.content`, `ai.chunks.content` | Owner-only by default. |
| Pipeline config | `ai.pipelines.config` | Owner-only by default. |

The extension does **not** protect against:
- A malicious extension owner (typically `postgres` superuser) — they can do anything anyway.
- Leaked container env vars (if `OPENAI_API_KEY` leaks, the whole system is compromised regardless of DB ACLs).

## SECURITY DEFINER rationale

All `ai.*` functions are `SECURITY DEFINER`. This is intentional:

- The function needs to read env vars (`OPENAI_API_KEY`) which are only readable
  inside the postgres process. SECURITY DEFINER makes the call run as the extension
  owner regardless of the calling user, so env vars are visible.
- search_path is pinned to `pg_catalog, public, ai, pg_temp` via `ALTER FUNCTION`
  to prevent search-path-based privilege escalation (HANDOFF.md §2:122).
- The actual API key value never enters PostgreSQL — only the env var name does.

## Default ACL state (after CREATE EXTENSION)

| Object | Default access |
|---|---|
| `ai.*` functions | PUBLIC EXECUTE — any role can call |
| `ai.endpoints`, `ai.models`, `ai.results`, `ai.pipelines`, `ai._outbox` | Owner-only |
| `ai.documents`, `ai.chunks` (created by pipeline-worker) | Owner of pipeline-worker connection |

For a single-tenant dev/analytics use case, defaults are fine — the extension owner
is typically the only role using these functions.

## Recommended restrictive ACL (multi-role deployment)

If you have a separate "app" role that should only be able to query (not register
new pipelines or ingest documents), apply `sql/restrict_acl.sql`:

```sql
\i sql/restrict_acl.sql
GRANT EXECUTE ON FUNCTION ai.search, ai.ask, ai.search_async, ai.ask_async TO my_app_role;
```

This:
- Revokes EXECUTE on all `ai.*` functions from PUBLIC.
- Grants EXECUTE only on read-side functions (search, ask, embed_raw) to PUBLIC.
- Write functions (`create_pipeline`, `ingest`, `embed_async`, `*_async`) must be
  explicitly granted per role.

## Audit checklist

Before exposing to production users, verify:

- [ ] All `ai.*` functions are SECURITY DEFINER (`pg_proc.prosecdef = true`)
- [ ] All `ai.*` functions have search_path pinned (check `pg_proc.proconfig`)
- [ ] `ai.endpoints.api_key_env` only contains env var **names**, never values
- [ ] Container env vars (`OPENAI_API_KEY`) are managed via secrets manager (not in repo)
- [ ] `.env` is in `.gitignore` (verified)
- [ ] Application role does not have CREATE on the `ai` schema (default)
- [ ] If exposing externally, `restrict_acl.sql` has been applied

## Known soft spots

1. **No row-level security on `ai.results`** — a multi-tenant deployment would
   need RLS so users only see their own results. Not implemented.
2. **No rate limiting** — a malicious caller could burn OpenAI tokens. Mitigation
   is at the OpenAI project level (spending limits).
3. **NOTIFY channel is shared** — anyone with EXECUTE on `pg_notify` can spoof
   pipeline events. Mitigation: don't expose `pg_notify` to untrusted roles.
