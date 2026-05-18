# pg_aidb 설계 철학

AI 기능을 PostgreSQL로 끌고 오려 할 때 마주치는 근본 문제들과,
그 문제를 바탕으로 내린 설계 결정의 기록이다.

---

## 1. AI-in-DB가 매력적으로 보이는 이유

데이터는 이미 PostgreSQL에 있다. 임베딩, 검색, 생성까지 SQL 한 줄로 처리할 수
있다면 애플리케이션 복잡도가 크게 줄어든다.

```sql
-- 이게 가능하다면 얼마나 편한가
SELECT ai.generate(
    query    => '이 문서를 요약해줘',
    pipeline => 'docs',
    model    => 'gpt-4o'
);
```

Oracle(26ai), EDB(aidb), Timescale(pgai)가 이 방향으로 제품을 만들고 있고,
Oracle은 아예 데이터베이스 이름에 "AI"를 넣었다.

---

## 2. PostgreSQL의 근본 제약

PostgreSQL은 **process-per-connection** 모델이다.

```
클라이언트 연결 1개 = OS 프로세스 1개
프로세스가 I/O 대기 중 = 그 커넥션은 완전히 멈춤
```

비동기 I/O(epoll, io_uring)나 스레드 기반 모델이 아니다.
하나의 백엔드 프로세스가 블록되면, 그 커넥션은 다른 어떤 일도 처리할 수 없다.

이 구조 때문에 `max_connections`는 단순한 설정값이 아니라
**동시에 블록될 수 있는 OS 프로세스의 상한**이다.

---

## 3. AI 워크로드의 특성

AI 연산은 모두 느리다.

| 연산 | 지연 시간 |
|------|----------|
| 쿼리 임베딩 (OpenAI API) | 200ms ~ 1s |
| 로컬 ONNX 임베딩 | 100ms ~ 수초 (모델/하드웨어 따라) |
| 리랭킹 | 100ms ~ 수초 |
| LLM 생성 | 1s ~ 30s+ |

"ONNX는 빠르다"는 말은 외부 API 대비 상대적인 것이지,
절대적으로 빠른 게 아니다. 어느 쪽이든 수백ms 이상이다.

---

## 4. 두 제약이 만났을 때

동시 요청 100개가 `ai.generate()`를 호출한다고 가정하면:

```
요청 100개
→ 백엔드 프로세스 100개가 전부 LLM 응답 대기 중 (1~30초)
→ 새 커넥션 거부
→ max_connections = 100이면 서비스 다운
```

PgBouncer를 써도 transaction mode에서는 LLM 호출 중 커넥션을 돌려받을 수 없다.
AI 함수 호출이 곧 트랜잭션 점유이기 때문이다.

**결론: 느린 I/O가 동기 쿼리 경로에 있으면, 어떤 설계를 해도 커넥션 풀이 병목이 된다.**

---

## 5. 타 제품의 접근과 한계

### Oracle 26ai

쿼리 타임 임베딩을 in-process ONNX로 해결했다.

```sql
VECTOR_EMBEDDING(model_name USING text AS data)
-- HTTP 없음 — ONNX Runtime이 DB 프로세스 안에서 실행
```

- 네트워크 I/O 없음 → 임베딩 단계 블로킹 해소
- HNSW 그래프를 SGA(공유 메모리)에 상주

**한계:**
- ONNX 변환 가능한 소형 오픈소스 모델만 가능
- GPT-4, Claude 등 외부 API → 여전히 HTTP, 여전히 블로킹
- LLM 생성 단계(1~30초)는 해결책 없음
- Exadata 없는 일반 배포에서 "AI-native" 성능은 과장
- 결국 고동시성 LLM 생성에서는 동일한 벽에 부딪힘

### EDB aidb

별도 해법 없이 블로킹을 수용한다.
파이프라인 빌드(인제스천)는 BGW 기반 비동기로 처리하지만,
쿼리 타임 임베딩/생성은 동기 HTTP 호출이다.

### Timescale pgai

두 경로를 명시적으로 분리했다.
- 인터랙티브: PL/Python + requests (블로킹 수용)
- 배치: PostgreSQL 밖 외부 워커 프로세스 (Vectorizer Worker)

외부 워커가 작업 큐를 폴링 → async HTTP → 결과를 PostgreSQL에 저장.
이게 현재까지 가장 솔직한 설계다.

### 공통점

세 제품 모두 **고동시성 실시간 AI 서빙을 타겟하지 않는다.**
실제 타겟은:
- DBA/데이터 분석가의 SQL 기반 AI 실험
- 배치성 인제스천 파이프라인
- 중저동시성 내부 도구

고동시성 사용자 서비스에서 LLM을 동기 쿼리 경로에 넣는 것은
어떤 DB 제품을 써도 안 되는 설계다.

---

## 6. MSA 프록시의 한계

"로컬 FastAPI 프록시를 앞에 두면 블로킹이 줄지 않냐"는 아이디어가 있다.

```
PostgreSQL 백엔드 → HTTP → 로컬 FastAPI → OpenAI API
```

로컬 라운드트립(1~5ms)으로 블로킹 시간을 줄일 수 있지만,
PostgreSQL 백엔드가 응답을 기다리는 구조는 동일하다.
FastAPI가 수만 건을 비동기로 처리해도,
PostgreSQL 백엔드는 결과를 받을 때까지 커넥션을 붙들고 있다.

**MSA는 외부 API 처리 용량을 늘리지만, 커넥션 점유 문제를 해결하지 않는다.**

---

## 7. LISTEN/NOTIFY의 가능성과 한계

비동기 2단계 패턴에서 결과 전달에 LISTEN/NOTIFY를 쓰는 방안이 있다.

```sql
-- 1단계: 즉시 반환
SELECT ai.search_async('질문') → request_id

-- NOTIFY 수신 후
SELECT * FROM ai.results WHERE id = request_id
```

**장점:**
- 커넥션이 AI 처리 중 해방됨
- 결과 오는 즉시 반응 (폴링 불필요)

**대용량에서의 한계:**
- LISTEN은 persistent 커넥션 필요 → PgBouncer transaction mode 불가
- 10,000 동시 사용자 = 10,000 persistent 커넥션 → max_connections 문제 재등장
- NOTIFY는 해당 채널 모든 리스너에게 브로드캐스트 → 대규모 thundering herd
- PostgreSQL 내부 NOTIFY 큐가 초당 수천 건에서 병목

**결론: LISTEN/NOTIFY는 수백 커넥션 수준까지 실용적이다.**
그 이상의 규모에서는 Redis Pub/Sub, WebSocket 서버 등 외부 레이어가 필요하다.

---

## 8. pg_aidb 설계 철학

위의 분석에서 도출한 원칙들이다.

### 원칙 1: PostgreSQL은 저장과 검색, AI 연산은 밖에서

```
[외부 AI 서비스 (embedding, LLM, reranking)]
              ↕ HTTP
[PostgreSQL + pg_aidb]
- 파이프라인 메타데이터
- 청킹/파싱 (CPU-only, HTTP 없음)
- 벡터 저장 (pgvector)
- ANN 검색 (로컬 SQL, 블로킹 없음)
- 요청 큐 / 결과 테이블
```

벡터 검색 자체는 PostgreSQL이 잘 할 수 있고, 블로킹도 없다.
느린 구간(임베딩, LLM)만 외부로 분리한다.

### 원칙 2: 이중 모드 API — 편의와 확장성을 모두

**동기 모드** — 개발, 분석, 저동시성 운영
```sql
SELECT ai.embed('텍스트', 'openai');
SELECT ai.search(query => '질문', pipeline => 'docs', top_k => 10);
SELECT ai.generate(query => '질문', pipeline => 'docs');
```
- 커넥션 점유를 명시적으로 수용
- EDB/Oracle과 동일한 트레이드오프
- 단, 이 한계를 문서에 숨기지 않음

**비동기 모드** — 고동시성 프로덕션
```sql
SELECT ai.embed_async('텍스트', 'openai')     → request_id
SELECT ai.search_async('질문', 'docs')         → request_id
SELECT ai.generate_async('질문', 'docs')       → request_id
```
- 즉시 반환, 커넥션 해방
- BGW 또는 외부 워커가 실제 처리
- 결과는 `ai.results` 테이블 + NOTIFY

개발 시엔 동기로 빠르게 실험하고,
프로덕션 배포 시 동일한 파이프라인 정의를 비동기로 전환.

### 원칙 3: 결과 테이블이 단일 진실 원천, NOTIFY는 힌트

```sql
-- BGW/워커 완료 시
INSERT INTO ai.results(id, status, data, finished_at)
    VALUES (req_id, 'done', result, now());
PERFORM pg_notify('ai_' || req_id::text, req_id::text);

-- 클라이언트 선택 1: NOTIFY (빠르지만 persistent 커넥션 필요)
LISTEN ai_<request_id>;

-- 클라이언트 선택 2: 폴링 (PgBouncer 호환)
SELECT * FROM ai.results WHERE id = $1 AND status = 'done';

-- NOTIFY가 유실돼도 테이블에 결과가 있으면 폴링으로 수신
```

NOTIFY는 "결과가 왔을 것 같다"는 신호일 뿐이다.
실제 결과는 항상 테이블에서 읽는다.
클라이언트가 NOTIFY/폴링 중 어느 것을 쓸지 자유롭게 선택한다.

대용량에서 NOTIFY가 한계에 부딪히면,
클라이언트는 NOTIFY를 포기하고 폴링으로만 전환해도 된다.
pg_aidb는 NOTIFY 사용을 강제하지 않는다.

### 원칙 4: 포지셔닝을 숨기지 않는다

pg_aidb는 고동시성 실시간 AI 서빙 엔진이 아니다.

**pg_aidb가 잘 하는 것:**
- SQL로 AI 파이프라인을 선언적으로 정의
- 인제스천 파이프라인 자동화 (BGW 기반)
- 데이터 로컬리티 활용 (데이터가 이미 PostgreSQL에 있을 때)
- 개발/분석 워크로드에서 AI 접근성 향상

**pg_aidb의 범위 밖:**
- 수만 건 동시 실시간 AI 서빙
- LLM 응답 스트리밍
- 모델 서빙 최적화 (배칭, 양자화 등)

수만 건 동시 처리가 필요하면:
앱 레이어에서 임베딩 후 벡터로 전달 → pg_aidb는 ANN 검색만 처리.

---

## 9. 무엇이 DB에 속하고 무엇이 밖에 속하는가

| 기능 | 위치 | 이유 |
|------|------|------|
| 벡터 저장 | DB (pgvector) | 데이터 로컬리티, 트랜잭션 |
| ANN 검색 | DB (로컬 SQL) | 빠름, 블로킹 없음, JOIN 가능 |
| 파이프라인 메타데이터 | DB | 일관성, SQL 관리 |
| 청킹/파싱 | DB (BGW) | 데이터 이동 없음 |
| 임베딩 생성 | 외부 API | 느림, 전용 서빙이 효율적 |
| LLM 생성 | 외부 API | 매우 느림, 전용 서빙 필수 |
| 리랭킹 | 외부 API | 느림, 배칭 최적화 필요 |
| 고동시성 알림 | 외부 (Redis 등) | NOTIFY 한계 |

---

## 10. 동기 모드 사용 시 권장 사항

동기 모드를 쓸 때 커넥션 점유를 완화하는 방법:

```
PgBouncer (session mode) + 타임아웃
→ 임베딩 500ms × 커넥션 100개 = 초당 200 요청 처리 가능
→ 이 이상이 필요하면 비동기 모드로 전환
```

타임아웃 설정:
```sql
SET statement_timeout = '30s';   -- LLM 호출 최대 대기
SET lock_timeout     = '5s';
```

---

이 문서는 pg_aidb 설계 과정에서 검토하고 기각한 아이디어들의 이유를 포함한다.
"왜 이렇게 하지 않았는가"가 "왜 이렇게 했는가"만큼 중요하다.
