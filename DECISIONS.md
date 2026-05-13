# Architecture Decision Records

---

## ADR-001: Background Worker vs 외부 서비스

### 쟁점

Auto-processing (새 데이터 감지 → embedding → vector index 갱신) 같은 비동기 작업을
PostgreSQL Background Worker로 처리할 것인가, 외부 서비스 책임으로 넘길 것인가.

### 배경

EDB AIDB는 `aidb.set_auto_preparer()`를 Background Worker로 구현한다.
우리는 extension을 인터페이스 레이어로만 유지하는 것을 원칙으로 하고 있다.

### 옵션 A: Background Worker (EDB 방식)

extension 안에 background worker를 두고 변경 감지 + 처리를 담당한다.

```
PostgreSQL
  └── pg_aidb extension
        ├── SQL 함수
        └── Background Worker
              ├── 변경 감지 (shared memory / logical replication)
              └── 외부 서비스 HTTP 호출
```

**장점:**
- 변경 감지가 DB 내부에서 일어나므로 누락 없음
- 외부 인프라 의존성 없음 (extension만 설치하면 동작)
- PostgreSQL 트랜잭션과 동기화 가능

**단점:**
- Worker crash 시 PostgreSQL postmaster가 재시작 결정
- extension 복잡도 증가 (shared memory, latch 관리)
- 스케일 아웃 불가 (워커 수가 PostgreSQL 설정에 종속)
- 모니터링/디버깅이 DB 로그에 묻힘

---

### 옵션 B: Trigger + NOTIFY → 외부 Worker 서비스

extension은 트리거와 pg_notify만 설치하고,
실제 처리는 LISTEN하는 외부 서비스가 담당한다.

```
PostgreSQL
  └── pg_aidb extension
        ├── SQL 함수
        └── Trigger → pg_notify('aidb_pipeline', payload)

외부 서비스 (pipeline-worker)
  ├── LISTEN aidb_pipeline
  ├── 수신 → 처리 (embedding, indexing)
  └── 결과 → DB write
```

**장점:**
- Extension은 진짜 인터페이스만 (SQL + trigger)
- Worker 서비스 독립 스케일링, 재시작 가능
- 모니터링/트레이싱이 서비스 레벨에서 가능
- Worker 언어/프레임워크 자유롭게 선택

**단점:**
- pipeline-worker 서비스를 별도로 운영해야 함
- NOTIFY는 트랜잭션 커밋 시 발행되지만, worker가 다운된 사이의 이벤트 유실 가능
- PostgreSQL 커넥션을 LISTEN 용도로 상시 점유

---

### 옵션 C: Trigger + NOTIFY + 이벤트 큐 (Outbox Pattern)

NOTIFY 유실 문제를 outbox 테이블로 보완한다.

```sql
-- extension이 설치하는 outbox 테이블
CREATE TABLE aidb._outbox (
    id          bigserial PRIMARY KEY,
    event_type  text NOT NULL,
    payload     jsonb NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    processed_at timestamptz
);

-- 트리거: outbox 기록 + notify
CREATE FUNCTION aidb.notify_pipeline() RETURNS trigger AS $$
BEGIN
    INSERT INTO aidb._outbox (event_type, payload)
    VALUES ('row_inserted', row_to_json(NEW)::jsonb);
    PERFORM pg_notify('aidb_pipeline', NEW.id::text);
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
```

worker는 NOTIFY로 빠르게 반응하되, 재시작 후에는 outbox에서 미처리 이벤트를 복구한다.

**장점:**
- 이벤트 유실 없음 (outbox가 WAL에 기록됨)
- Worker 다운/재시작에 안전
- NOTIFY는 힌트 역할만 (없어도 outbox 폴링으로 복구)

**단점:**
- outbox 테이블 관리 (정리 job 필요)
- 구현 복잡도 증가

---

### 옵션 D: Logical Replication (CDC)

pg_aidb가 logical replication slot을 만들고,
외부 worker가 WAL을 직접 소비한다.

```
PostgreSQL WAL
  └── logical replication slot (aidb_slot)
        └── pipeline-worker (WAL 소비)
              ├── INSERT/UPDATE 감지
              └── 처리
```

**장점:**
- 가장 안정적. WAL 기반이라 이벤트 유실 원천 차단
- 트랜잭션 경계 정확히 반영
- 기존 Debezium/pglogical 생태계 활용 가능

**단점:**
- Replication slot 미소비 시 WAL 무한 증가 → 디스크 위험
- worker가 완전히 죽으면 slot 수동 삭제 필요
- 운영 복잡도 높음
- 일반 사용자에게 권한 부여 복잡

---

### 고려 사항

**배포 타겟이 무엇인가?**
- 단순 설치 (`CREATE EXTENSION`)만으로 동작해야 한다면 → 옵션 A
- Docker Compose / K8s 환경이 전제된다면 → 옵션 B, C, D

**이벤트 유실이 허용되는가?**
- 허용 안 됨 → 옵션 C (Outbox) 또는 D (CDC)
- 허용 가능 (재처리 트리거 있음) → 옵션 B

**운영 복잡도를 얼마나 감수할 수 있는가?**
- 낮게 유지 → 옵션 A 또는 B
- 복잡도 감수 가능 → 옵션 C 또는 D

---

### EDB AIDB는 왜 Background Worker를 썼는가

EDB AIDB는 두 가지 배포 모드를 지원한다.

- **Hybrid Manager 있음**: AI Factory 서비스들이 K8s에서 동작. extension은 인터페이스만.
- **Hybrid Manager 없음 (standalone)**: `CREATE EXTENSION aidb` 하나로 모든 게 동작해야 함. Background Worker가 auto-processing을 담당.

Background Worker를 extension에 넣은 이유는 **standalone 설치를 지원하기 위해서**다.
Hybrid Manager가 있으면 그 Worker는 사실상 놀게 된다.

우리는 standalone을 지원하지 않는다. Docker Compose가 최소 요구사항이므로
Background Worker를 extension에 둘 이유가 없다.

### 결론

**옵션 C (Trigger + NOTIFY + Outbox Pattern) 채택.**

- 배포 최소 요구사항: **Docker Compose** (standalone 미지원)
- Background Worker를 extension에 두지 않는다
- auto-processing 포함 모든 비동기 작업은 외부 서비스(pipeline-worker) 책임
- extension 역할: SQL 함수 + Trigger + NOTIFY + Outbox 테이블 설치까지

### 미결 사항

- [ ] 이벤트 유실 허용 여부 (Outbox로 충분한지, CDC가 필요한지)
- [ ] Outbox 테이블을 extension이 관리할 것인지, 사용자 스키마에 둘 것인지

---

## ADR-003: RAG 파이프라인 책임 분리 원칙

### 결론

**RAG 파이프라인은 최대한 외부 플랫폼 책임으로 만든다. Extension은 단순 호출만 한다.**

### 근거

SQL 인터페이스(`ai.retrieve()`, `ai.predict()` 등)를 호출하는 주체는 사람이 아니라 애플리케이션이다. 애플리케이션 입장에서 SQL 호출과 SDK 호출의 차이는 없다. SQL 인터페이스가 편리하다는 주장은 분석가가 직접 쿼리하는 케이스에만 유효하다.

RAG/LLM 워크로드는 DB, App, GPU, Storage가 각자의 전문성으로 독립적으로 스케일링되고 장애 도메인이 분리되어야 한다. DB 안으로 GPU 워크로드를 끌고 오면 스케일링 단위가 충돌하고 장애 도메인이 합쳐진다.

PostgresML은 in-process ML로 $4.7M을 받았으나 LLM 시대에 설계가 맞지 않아 2025년 서비스 종료했다. Oracle 26ai, EDB AIDB의 in-DB AI는 기술적 필요보다 벤더 락인에 가깝다.

### extension이 하는 것

- model_registry 조회
- 플랫폼 endpoint 호출 (pg_net)
- 결과 반환

### 플랫폼(외부 서비스)이 하는 것

- chunking
- embedding
- vector indexing
- retrieval
- reranking
- LLM 호출
- document ingestion
- pipeline orchestration

---

## ADR-002: pg_aidb Extension vs Pure UDF

### 쟁점

pg_aidb를 컴파일된 extension (.so)으로 만들 것인가,
SQL 함수(UDF) + pg_net 조합으로만 구성할 것인가.

### 결론

**Extension으로 유지한다.**

이유:
- Background Worker 옵션을 열어두려면 extension이 필수
- GUC 변수 (`pg_aidb.vector_backend` 등) 등록은 extension만 가능
- Trigger 함수를 C/Rust로 작성하면 PL/pgSQL 대비 성능 우위
- 향후 pg_cuvs 연동 시 타입 시스템 통합 용이

PL/pgSQL UDF만으로 갈 경우 잃는 것:
- GUC 변수 (config 테이블로 대체 가능하나 우아하지 않음)
- Background Worker
- C 레벨 hook 가능성
