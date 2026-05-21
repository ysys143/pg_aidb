# GPU 가속 전략 — pg_aidb × pg_cuvs

최종 업데이트: 2026-05-22

---

## 1. 왜 GPU가 필요한가

pg_aidb의 RAG 파이프라인에서 병목은 두 곳이다.

| 단계 | 현재 구현 | 규모 한계 |
|---|---|---|
| 벡터 유사도 검색 | pgvector HNSW (CPU) | ~수백만 벡터에서 레이턴시 급증 |
| MMR 후보 재정렬 | numpy cosine (Python, CPU) | fetch_k가 크면 O(n²) 비교 |

프로덕션 RAG 워크로드는 일반적으로 수백만~수십억 청크를 보유하고, p99 레이턴시 요구가 100ms 이하다. CPU HNSW는 이 교차점에서 한계를 드러낸다.

pg_cuvs (GPU-accelerated vector search for PostgreSQL, NVIDIA cuVS 기반)는 이 병목을 투명하게 해결한다:

- SQL 쿼리 변경 없음 — 동일한 `<=>` 연산자
- pgvector HNSW 대비 QPS 5x 이상 (1M+ 벡터 L4 GPU 기준)
- PostgreSQL이 실행 주체, GPU는 후보 생성기

---

## 2. pg_cuvs 아키텍처 요약

```
┌─────────────────────────────────────────┐
│  PostgreSQL                              │
│  ai.search / ai.search_hybrid / ai.ask  │
│         │                               │
│         │ pgvector <=> 연산자           │
│         ▼                               │
│  pg_cuvs_server (GPU Sidecar Daemon)    │
│  - CUDA 컨텍스트 단일화                 │
│  - CAGRA 인덱스 VRAM 상주               │
│  - Shared Memory IPC                    │
│         │                               │
│         │ TID + 거리 반환               │
│         ▼                               │
│  PostgreSQL heap 접근 (MVCC, 권한, 조인) │
└─────────────────────────────────────────┘
```

GPU가 죽으면 PostgreSQL은 CPU 경로(HNSW)로 자동 Graceful Degradation.

---

## 3. pg_aidb 쿼리 패턴별 GPU 가속 효과

### 3.1 `ai.search` — dense-only cosine

```sql
SELECT * FROM ai.search('query', 'pipeline', top_k := 5);
```

내부적으로 `ai.chunks.embedding <=> query_vec` cosine 검색.
pg_cuvs 설치 후 `CREATE INDEX USING cagra (embedding vector_cosine_ops)`로 교체 시 GPU 가속.
**SQL 변경 없음.** Cost Model이 벡터 수에 따라 CPU/GPU 자동 선택.

### 3.2 `ai.search_hybrid` — BM25 + dense RRF

```sql
SELECT * FROM ai.search_hybrid('query', 'pipeline', top_k := 5);
```

- **Dense branch**: `ai.chunks.embedding <=>` — GPU 가속 적용
- **Sparse branch**: `content_tsv @@ plainto_tsquery` + `content <@> bm25query` — pg_textsearch가 CPU에서 처리. GPU와 독립.
- **RRF fusion**: SQL CTE, PostgreSQL 처리

두 경로가 분리되어 있어 dense만 GPU로 교체 가능.

### 3.3 `ai.search_mmr` — MMR 재정렬

현재 rag service Python numpy에서 `fetch_k` 후보를 CPU로 greedy selection.
pg_cuvs Phase 2에서 GPU-side MMR 구현이 가능해지면 이 레이어를 GPU로 이전 가능.
단기적으로는 fetch_k를 늘려도 GPU 후보 생성이 빠르면 전체 레이턴시 유지 가능.

### 3.4 `ai.ask` — LLM 생성

벡터 연산 없음. GPU 가속 비적용. LLM API 레이턴시가 지배적.

---

## 4. 통합 로드맵

pg_cuvs는 [3단계 로드맵](https://github.com/ysys143/pg_cuvs)으로 개발 중이다.
pg_aidb의 통합 전략은 pg_cuvs 단계에 맞춰 진행한다.

### 4.1 단기 — pg_cuvs Phase 1 완료 시점 (CPU fallback 확보 후)

**pg_aidb 변경 사항**:

1. `deploy/docker/Dockerfile.pg` — pg_cuvs 설치 레이어 추가
   ```dockerfile
   # GPU 환경 전용 이미지 (Dockerfile.pg.gpu)
   FROM pgvector/pgvector:pg17
   RUN mamba install -n base libcuvs ...
   COPY extensions/pg_cuvs /tmp/pg_cuvs
   RUN cd /tmp/pg_cuvs && make && make install
   ```

2. `services/pipeline-worker/sql/schema.sql` — CAGRA 인덱스 옵션 추가 (기존 HNSW와 공존)
   ```sql
   -- GPU 환경 감지 후 조건부 생성
   DO $$
   BEGIN
     IF EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_cuvs') THEN
       CREATE INDEX IF NOT EXISTS ai_chunks_cagra_idx
           ON ai.chunks USING cagra (embedding vector_cosine_ops);
     END IF;
   END $$;
   ```

3. `docker-compose.yml` — GPU 서비스 profile 추가
   ```yaml
   pg-gpu:
     build:
       context: ./deploy/docker
       dockerfile: Dockerfile.pg.gpu
     deploy:
       resources:
         reservations:
           devices:
             - driver: nvidia
               count: 1
               capabilities: [gpu]
   ```

**pg_aidb 코드 변경 없음**: rag service의 SQL 쿼리는 `<=>` 연산자를 그대로 사용. pg_cuvs Cost Model이 CAGRA 인덱스를 자동 선택.

### 4.2 중기 — pg_cuvs Phase 2 (Tiered Storage 확보 후)

- 대규모 컬렉션(수천만 청크)에서 `ai.chunks` HNSW → CAGRA + DiskANN 조합
- `ai.search_mmr`의 fetch_k 기본값 상향 (20 → 100+) — GPU가 후보 생성 담당
- `pg_stat_gpu_search` 뷰를 ai.usage_v1에 연결 — 토큰 비용 외 GPU 커널 시간 추적

### 4.3 장기 — pg_cuvs Phase 3 (S3 인덱스 스냅샷)

- pg_aidb의 컬렉션 = pg_cuvs의 인덱스 파티션 단위로 매핑
- 컬렉션 archival: Hot(CAGRA/VRAM) → Warm(DiskANN/NVMe) → Cold(S3 snapshot) 자동 계층화
- `ai.create_pipeline` config에 `storage_tier: hot|warm|cold` 옵션 추가

---

## 5. 인덱스 전략

### 현재 (CPU)

```sql
CREATE INDEX ai_chunks_embedding_idx
    ON ai.chunks USING hnsw (embedding vector_cosine_ops);
```

### GPU 환경 (pg_cuvs Phase 1+)

```sql
-- CAGRA: 핫 데이터, VRAM 수용 가능한 규모 (<~35M @ fp16/1536d)
CREATE INDEX ai_chunks_cagra_idx
    ON ai.chunks USING cagra (embedding vector_cosine_ops);

-- DiskANN: 대용량, NVMe 기반 (Phase 2)
CREATE INDEX ai_chunks_diskann_idx
    ON ai.chunks USING diskann (embedding vector_cosine_ops);
```

pgvector HNSW, CAGRA, DiskANN 모두 `<=>` 연산자 호환. Cost Model이 자동 선택.

---

## 6. 개발 원칙

**CPU-first, GPU-optional**: pg_aidb의 모든 기능은 GPU 없이도 완전히 동작한다.
GPU는 성능 레이어이지 기능 레이어가 아니다.

**인터페이스 고정**: `ai.search`, `ai.search_hybrid`, `ai.search_mmr` SQL 시그니처는
pg_cuvs 통합과 무관하게 고정. 가속은 인덱스 레벨에서 투명하게 적용.

**격리된 Dockerfile**: `Dockerfile.pg` (CPU, 현재)와 `Dockerfile.pg.gpu` (GPU 선택적)를 분리.
CI는 CPU 경로로 실행. GPU 테스트는 별도 환경에서 실행.

---

## 7. 벤치마크 목표 (통합 후)

| 지표 | CPU (현재) | GPU 목표 |
|---|---|---|
| `ai.search` p50 @ 1M 청크 | ~20ms | <5ms |
| `ai.search` p99 @ 1M 청크 | ~80ms | <15ms |
| `ai.search_mmr` p50 (fetch_k=100) | ~100ms | <20ms |
| 인제스트 throughput (embedding 제외) | 기준 | 1.5x (배치 insert 병렬화) |

기준값은 `design/BENCHMARKS.md`의 실측값에서 갱신.

---

## 9. GCP GPU VM 개발 환경 구성

로컬 맥에 NVIDIA GPU가 없으므로 GCP 인스턴스에서 빌드/테스트를 진행한다.
pg_aidb 개발에서 얻은 교훈을 그대로 적용한다.

### 9.1 기본 구조

```
로컬 맥 (코드 편집)           GCP GPU VM (빌드 + 테스트)
────────────────────         ──────────────────────────
VSCode Remote SSH ─────────→ CUDA + PostgreSQL + pg_cuvs
make 로컬 타깃 실행 ─────────→ SSH로 원격 실행 + 결과 스트리밍
```

### 9.2 pg_aidb 교훈 → pg_cuvs 적용

**교훈 1: named volume 함정** (pg_aidb에서 `.so`가 옛 볼륨에 잔류해 `down -v`를 반복)

pg_cuvs는 Docker 볼륨 대신 **bind mount + 명시적 copy**로 해결:
```makefile
define INSTALL_EXT
    sudo cp pg_cuvs.so $(PG_LIBDIR)/
    sudo cp pg_cuvs.control $(PG_EXTDIR)/
    sudo cp sql/pg_cuvs--*.sql $(PG_EXTDIR)/
endef
```

**교훈 2: 백그라운드 + pipe 잘못 쓰면 출력 0바이트** (pg_aidb에서 `| tail -40`으로 중간 출력 차단)

모든 GCP 원격 명령은 `ssh -tt` + `tee`로 실시간 스트리밍:
```makefile
gpu-build:
    ssh -tt $(GCP_VM) "cd ~/pg_cuvs && make 2>&1 | tee /tmp/build.log"
```

**교훈 3: make가 유일한 진입점** (pg_aidb에서 make 우회 → BOOTSTRAP_PG 누락)

GCP 원격 실행도 전부 make 타깃으로 캡슐화:
```makefile
gpu-test:
    ssh $(GCP_VM) "cd ~/pg_cuvs && make test"

gpu-bench:
    ssh $(GCP_VM) "cd ~/pg_cuvs && make benchmark 2>&1" | tee design/bench_$(shell date +%Y%m%d).log
```

**교훈 4: `.env`로 환경 변수 관리** (gitignore, 실제 값 커밋 금지)

```bash
# .env.gpu
GCP_VM=user@34.xxx.xxx.xxx
GCP_INSTANCE=pg-cuvs-dev
GCP_ZONE=us-central1-a
CUDA_VERSION=12.4
CONDA_ENV=cuvs_dev
```

### 9.3 GCP 인스턴스 스펙

- `g2-standard-4` — NVIDIA L4 1개, 4 vCPU, 16GB RAM, 100GB SSD
- OS: Ubuntu 22.04 LTS
- **Spot 인스턴스** 권장 — 비용 60~70% 절감, 개발 중 인터럽트 허용

### 9.4 Makefile 라이프사이클

```makefile
# VM 시작/중지 — idle 비용 방지
vm-start:
    gcloud compute instances start $(GCP_INSTANCE) --zone $(GCP_ZONE)
    @until ssh -o ConnectTimeout=5 $(GCP_VM) true 2>/dev/null; do sleep 3; done
    @echo "VM ready"

vm-stop:
    gcloud compute instances stop $(GCP_INSTANCE) --zone $(GCP_ZONE)

# 코드 동기화
sync:
    rsync -avz --exclude '.git' --exclude 'build' . $(GCP_VM):~/pg_cuvs/
```

### 9.5 테스트 계층 분리

| 계층 | 실행 위치 | 명령 | GPU 필요 |
|---|---|---|---|
| C 컴파일 확인 | 로컬 또는 GCP | `make check-syntax` | 불필요 |
| CPU fallback 테스트 | GCP (`enable_cuvs=off`) | `make test-cpu` | 불필요 |
| GPU 정확성 테스트 | GCP | `make test-gpu` | 필요 |
| 벤치마크 | GCP | `make benchmark` | 필요 |

`make test-cpu`는 `SET enable_cuvs = off`로 비활성화 — pg_cuvs의 Graceful Degradation을 활용해 GPU 없이도 CPU path 검증.

### 9.6 일일 워크플로우

```bash
# 작업 시작 — VM 켜고 코드 동기화
make vm-start sync

# 개발 루프 (로컬 편집 → GCP 빌드 → 결과 확인)
make sync gpu-build gpu-test

# 벤치마크 — 결과 로컬 로그에 자동 저장
make gpu-bench   # design/bench_YYYYMMDD.log에 tee

# 작업 종료 — VM 종료
make vm-stop
```

---

## 8. 참고

- [pg_cuvs](https://github.com/ysys143/pg_cuvs) — GPU 사이드카 모델, CAGRA/DiskANN AM
- [RAPIDS cuVS](https://github.com/rapidsai/cuvs) — CAGRA, IVF-Flat, Brute Force GPU 구현
- [pgvectorscale](https://github.com/timescale/pgvectorscale) — CPU DiskANN 참고 구현
- [PG-Strom](https://github.com/heterodb/pg-strom) — GPU 사이드카 아키텍처 원형
- `design/BENCHMARKS.md` — pg_aidb 현재 성능 기준값
