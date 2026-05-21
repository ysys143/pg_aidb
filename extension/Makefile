EXTENSION   = pg_aidb
DATA        = $(wildcard sql/pg_aidb--*.sql)

# Load .env from repo root if it exists (never errors if absent).
# 'export' propagates all variables to sub-make and docker compose.
-include ../.env
export

# embed_mock is pg_regress style (opt-in via installcheck-mock).
# rag_* are direct psql (run-rag-*), excluded from default installcheck.
MOCK_TESTS   = embed_mock
DIRECT_TESTS = rag_e2e rag_parse rag_async dump_seed dump_verify
_OPT_IN      = $(MOCK_TESTS) $(DIRECT_TESTS)
TESTS        = $(patsubst test/sql/%.sql,%, \
                 $(filter-out $(patsubst %,test/sql/%.sql,$(_OPT_IN)), \
                   $(wildcard test/sql/*.sql)))
REGRESS      = $(TESTS)
REGRESS_OPTS = --inputdir=test --outputdir=test

PG_CONFIG  ?= pg_config
PGXS       := $(shell $(PG_CONFIG) --pgxs)
include $(PGXS)

.PHONY: installcheck-mock pgrx-test pgrx-install \
        run-rag-mock run-rag-real \
        run-rag-parse-mock run-rag-parse-real \
        run-rag-async-real \
        verify-dump-restore

# pg_regress mock suite (embed_mock only — needs mock server).
installcheck-mock: install
	$(MAKE) installcheck REGRESS="$(REGRESS) $(MOCK_TESTS)"

# pgrx unit tests (must run as dev user; CARGO_TARGET_DIR on native FS to avoid Unix socket bug).
pgrx-test:
	docker compose exec builder bash -c '\
		mkdir -p /home/dev/target && chown -R dev:dev /home/dev/target && \
		su -s /bin/bash dev -c "\
			export HOME=/home/dev CARGO_HOME=/home/dev/.cargo PGRX_HOME=/home/dev/.pgrx; \
			export CARGO_TARGET_DIR=/home/dev/target; \
			export PATH=/home/dev/.cargo/bin:\$$PATH; \
			cd /workspace/extension && cargo pgrx test pg17"'

# Install extension into local PostgreSQL via pgrx (not Docker).
pgrx-install:
	cargo pgrx install --pg-config $(PG_CONFIG)

# ============================================================
# Shared bootstrap helpers for direct-psql E2E tests
# ============================================================
# DROP EXTENSION CASCADE drops the ai schema (and pipeline-worker's
# ai.documents/ai.chunks). pipeline-worker recreates them on restart.
define BOOTSTRAP_PG
	docker compose exec builder bash -c "\
		psql -h pg -U postgres -d aidb -c 'DROP EXTENSION IF EXISTS pg_aidb CASCADE' 2>&1; \
		cargo pgrx install --pg-config /usr/bin/pg_config 2>&1 && \
		psql -h pg -U postgres -d aidb -c 'CREATE EXTENSION pg_aidb' 2>&1"
endef

define WAIT_PIPELINE_WORKER
	docker compose exec builder bash -c "\
		until curl -sf http://pipeline-worker:8001/health >/dev/null 2>&1; do sleep 2; done"
endef

define RUN_PSQL
	docker compose exec builder bash -c "\
		psql -h pg -U postgres -d aidb -e -f /workspace/extension/test/sql/$(1)"
endef

# ============================================================
# Direct psql E2E targets (rag_e2e.sql, rag_parse.sql, rag_async.sql)
# ============================================================

run-rag-mock:
	OPENAI_API_KEY=mock-key OPENAI_BASE_URL=http://mock:8080/v1/ \
		docker compose up -d --force-recreate pg mock builder
	$(BOOTSTRAP_PG)
	OPENAI_API_KEY=mock-key OPENAI_BASE_URL=http://mock:8080/v1/ \
		docker compose up -d --force-recreate pipeline-worker rag
	$(WAIT_PIPELINE_WORKER)
	$(call RUN_PSQL,rag_e2e.sql)

run-rag-real:
	@test -n "$(OPENAI_API_KEY)" || (echo "ERROR: OPENAI_API_KEY not set — add it to .env"; exit 1)
	docker compose up -d --force-recreate pg builder
	$(BOOTSTRAP_PG)
	docker compose up -d --force-recreate pipeline-worker rag
	$(WAIT_PIPELINE_WORKER)
	$(call RUN_PSQL,rag_e2e.sql)

run-rag-context-real:
	@test -n "$(OPENAI_API_KEY)" || (echo "ERROR: OPENAI_API_KEY not set — add it to .env"; exit 1)
	docker compose up -d --force-recreate pg builder
	$(BOOTSTRAP_PG)
	docker compose up -d --force-recreate pipeline-worker rag
	$(WAIT_PIPELINE_WORKER)
	$(call RUN_PSQL,rag_context.sql)

run-rag-mmr-real:
	@test -n "$(OPENAI_API_KEY)" || (echo "ERROR: OPENAI_API_KEY not set — add it to .env"; exit 1)
	docker compose up -d --force-recreate pg builder
	$(BOOTSTRAP_PG)
	docker compose up -d --force-recreate pipeline-worker rag
	$(WAIT_PIPELINE_WORKER)
	$(call RUN_PSQL,rag_mmr.sql)

run-rag-filter-real:
	@test -n "$(OPENAI_API_KEY)" || (echo "ERROR: OPENAI_API_KEY not set — add it to .env"; exit 1)
	docker compose up -d --force-recreate pg builder
	$(BOOTSTRAP_PG)
	docker compose up -d --force-recreate pipeline-worker rag
	$(WAIT_PIPELINE_WORKER)
	$(call RUN_PSQL,rag_filter.sql)

run-rag-hybrid-real:
	@test -n "$(OPENAI_API_KEY)" || (echo "ERROR: OPENAI_API_KEY not set — add it to .env"; exit 1)
	docker compose up -d --force-recreate pg builder
	$(BOOTSTRAP_PG)
	docker compose up -d --force-recreate pipeline-worker rag
	$(WAIT_PIPELINE_WORKER)
	$(call RUN_PSQL,rag_hybrid.sql)

run-rag-parse-mock:
	OPENAI_API_KEY=mock-key OPENAI_BASE_URL=http://mock:8080/v1/ \
		docker compose up -d --force-recreate pg mock builder
	$(BOOTSTRAP_PG)
	OPENAI_API_KEY=mock-key OPENAI_BASE_URL=http://mock:8080/v1/ \
		docker compose up -d --force-recreate pipeline-worker rag
	$(WAIT_PIPELINE_WORKER)
	$(call RUN_PSQL,rag_parse.sql)

run-rag-parse-real:
	@test -n "$(OPENAI_API_KEY)" || (echo "ERROR: OPENAI_API_KEY not set — add it to .env"; exit 1)
	docker compose up -d --force-recreate pg builder
	$(BOOTSTRAP_PG)
	docker compose up -d --force-recreate pipeline-worker rag
	$(WAIT_PIPELINE_WORKER)
	$(call RUN_PSQL,rag_parse.sql)

run-rag-async-real:
	@test -n "$(OPENAI_API_KEY)" || (echo "ERROR: OPENAI_API_KEY not set — add it to .env"; exit 1)
	docker compose up -d --force-recreate pg builder
	$(BOOTSTRAP_PG)
	docker compose up -d --force-recreate pipeline-worker rag
	$(WAIT_PIPELINE_WORKER)
	$(call RUN_PSQL,rag_async.sql)

# C6: verify pg_extension_config_dump('ai.*') survives pg_dump/restore round-trip.
# Without that flag, pg_dump skips extension-owned tables and we'd lose user data.
verify-dump-restore:
	docker compose up -d pg builder
	$(BOOTSTRAP_PG)
	$(call RUN_PSQL,dump_seed.sql)
	docker compose exec builder bash -c "\
		pg_dump -h pg -U postgres aidb > /tmp/aidb.dump && \
		psql -h pg -U postgres -d postgres -c 'DROP DATABASE IF EXISTS aidb_dump_target' && \
		psql -h pg -U postgres -d postgres -c 'CREATE DATABASE aidb_dump_target' && \
		psql -h pg -U postgres -d aidb_dump_target -q -f /tmp/aidb.dump > /tmp/restore.log 2>&1"
	docker compose exec builder bash -c "\
		psql -h pg -U postgres -d aidb_dump_target -e \
		  -f /workspace/extension/test/sql/dump_verify.sql"
	docker compose exec builder bash -c "\
		psql -h pg -U postgres -d postgres -c 'DROP DATABASE aidb_dump_target' && \
		psql -h pg -U postgres -d aidb -c \"\
			DELETE FROM ai.results   WHERE data->>'op'='dump-test'; \
			DELETE FROM ai.pipelines WHERE name='dump-test-pl'; \
			DELETE FROM ai.models    WHERE name='dump-test-model'; \
			DELETE FROM ai.endpoints WHERE name='dump-test-ep';\""
	@echo "[OK] pg_dump round-trip verified"
