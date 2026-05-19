# pg_aidb 구현 핸드오프

pg_extension/examples 에서 C/Rust/Python extension을 직접 구현하며 얻은 패턴과 함정.
pg_aidb 구현 시작 전에 반드시 읽어야 할 문서다.

참조 레포: `~/Documents/GitHub/pg_extension`
설계 문서: `~/Documents/Github/pg_aidb/design/`

---

## 1. pgrx 0.18 핵심 패턴

### 참조 파일
`examples/rust/test/src/lib.rs`

### `#[pg_extern]` 기본 패턴 (lib.rs:10~50)

```rust
use pgrx::prelude::*;
pg_module_magic!();

#[pg_extern]
fn my_func(input: &str) -> String {
    format!("result: {}", input)
}

// NULL 허용 인자
#[pg_extern]
fn with_default(
    input: &str,
    model: default!(&str, "'gpt-5.4'"),
) -> String { ... }
```

### `extension_sql!` requires 순서 (lib.rs:287~296)

`requires`가 없거나 순서가 틀리면 "function not found" 에러.
string literal로 SQL 블록을 참조하고, Rust 식별자로 함수를 참조한다.

```rust
#[pg_extern(immutable, strict)]
fn product_sfunc(state: i64, val: i64) -> i64 { state * val }

extension_sql!(
    r#"CREATE AGGREGATE product_agg(int8) (
        sfunc = product_sfunc, stype = int8, initcond = '1'
    );"#,
    name = "product_agg_aggregate",
    requires = [product_sfunc],   // Rust 식별자
);
```

ai.results 테이블, ai.models 등 테이블을 만들고 그 테이블에 의존하는
함수를 선언할 때 requires 순서가 핵심이다.

### `#[pg_aggregate]` + primitive state type 버그 (lib.rs:281)

pgrx 0.18에서 state type이 i64 같은 primitive면 컴파일 에러.
대신 sfunc + `extension_sql!` 조합으로 우회한다.

### SPI 중첩 3레벨 문제 (LESSONS_LEARNED.md)

`#[pg_test]` 안에서 이미 SPI 레벨 1이다.
`Spi::get_one("SELECT * FROM my_srf()")` 형태는 레벨 3까지 중첩되어
내부 SPI 쿼리가 0행 반환하는 현상이 생긴다.

```rust
// 피할 것
let result = Spi::get_one::<i64>("SELECT count(*) FROM ai_search(...)");

// 권장: Rust 함수 직접 호출
let result = crate::ai_search_fn(...);
```

---

## 2. pgvector 의존성 처리

### 참조 파일
- `examples/rust/test/src/lib.rs:119~122` (주석)
- `examples/rust/test/test.control`
- `examples/c/test/sql/test.sql:114~120`

### pgrx 테스트 환경에서 CASCADE SIGSEGV

`test.control`에 `requires = 'vector'`를 넣으면
`cargo pgrx test` 실행 시 `CREATE EXTENSION pg_aidb CASCADE`가
multi-user 테스트 서버에서 SIGSEGV를 일으킨다.

gdb 코어 덤프에서 스택 손상이 확인됐으며 pgrx 테스트 프레임워크 한계다.
프로덕션 배포(pg_regress 기반)에서는 정상 동작한다.

**해결책: `requires = 'vector'` 제거, pgvector를 소프트 의존성으로 취급**

```toml
# pg_aidb.control
# requires = 'vector'  ← 이 줄 없애야 함
```

`RETURNS vector` 래퍼 함수도 extension_sql!에서 제거하고 사용자가
수동 생성하도록 주석으로 안내:

```rust
// NOTE: ai.embed() RETURNS vector wrapper omitted.
// pgrx test framework crashes when vector is CASCADE-loaded.
// Users with pgvector installed can create the wrapper manually:
//   CREATE FUNCTION ai.embed(...) RETURNS vector
//   AS $$ SELECT ai.embed_raw(...)::vector $$;
```

### SECURITY DEFINER + pgvector vector 타입

`vector` 타입은 `public` 스키마에 등록된다.
SECURITY DEFINER 함수에 `SET search_path`를 지정하면 `public`이 빠져서
`vector` 타입을 못 찾는다.

```sql
-- 틀림: vector 못 찾음
SET search_path = pg_catalog, ai, pg_temp

-- 맞음: public 포함 필수
SET search_path = pg_catalog, public, ai, pg_temp
```

참조: `examples/c/test/sql/test.sql:119`

---

## 3. HTTP 클라이언트 패턴

### 참조 파일
- `examples/c/test/src/test.c:382~620` (C libcurl 패턴)
- `examples/rust/test/src/lib.rs:140~189` (Rust reqwest 패턴)

pg_aidb extension은 Rust이므로 reqwest를 쓴다.
C 예제의 3-phase BGW 패턴은 참조용이다.

### Rust reqwest 동기 HTTP (Cargo.toml)

```toml
[dependencies]
pgrx = "=0.18.0"
reqwest = { version = "0.12", features = ["blocking", "json"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

```rust
// 동기 호출 (sync API용)
let client = reqwest::blocking::Client::builder()
    .timeout(std::time::Duration::from_secs(30))
    .build()?;

let response: serde_json::Value = client
    .post(&url)
    .bearer_auth(&api_key)
    .json(&body)
    .send()?
    .json()?;
```

### 3-phase 패턴: HTTP는 반드시 트랜잭션 밖에서

C 참조: `examples/c/test/src/test.c:496~680`

```
Phase 1 (inside tx):  SPI → credentials 읽기 → TopMemoryContext로 복사
Phase 2 (outside tx): HTTP 호출 (blocking OK, 트랜잭션 없음)
Phase 3 (inside tx):  SPI → 결과 UPSERT
```

Rust에서는 이 구조가 자연스럽게 Rust 함수 호출 순서로 표현된다.
트랜잭션 경계는 `Spi::connect()` / `Spi::run()` 블록으로 관리.

### C libcurl 참조 (pg_aidb에서 직접 쓰지는 않지만)

- `HttpBuf` + `http_write_cb` 패턴: `test.c:384~399`
- embed_raw POST 패턴: `test.c:1177~1270`
- health_monitor 3-phase: `test.c:517~685`

---

## 4. BGW 패턴 (참조용, extension에서 직접 쓰지 않음)

ADR-001에서 BGW를 extension 밖(pipeline-worker)으로 뺐다.
그러나 향후 필요 시 참조:

### 참조 파일
- `examples/c/test/src/test.c:382~730` (C BGW 전체)
- `examples/rust/test/src/lib.rs:299~388` (Rust BGW)

### bgw_extra로 파라미터 전달

```c
// 등록 측 (test.c:716)
snprintf(worker.bgw_extra, BGW_EXTRALEN, "%d", interval_sec);

// 워커 측 (test.c:520)
long interval_ms = (long)(atoi(MyBgworkerEntry->bgw_extra) * 1000L);
```

### pgrx에서 BGW 단위 테스트 불가

`#[pg_test]`에서 `bgw_start()` 호출 시 SIGABRT.
검증은 C extension의 pg_regress로:
`examples/c/test/test/sql/bgworker.sql` 참조.

---

## 5. 테스트 패턴

### 참조 파일
- `examples/c/test/Makefile`
- `examples/c/test/test/sql/health_monitor.sql`
- `examples/c/test/test/expected/health_monitor.out`
- `examples/mock_openai.py`

### 외부 API 의존 테스트 분리

```makefile
# Makefile (examples/c/test/Makefile:12~18 참조)
MOCK_TESTS = test/sql/llm_mock.sql test/sql/health_monitor.sql
TESTS      = $(filter-out $(MOCK_TESTS), $(wildcard test/sql/*.sql))
REGRESS    = $(patsubst test/sql/%.sql,%,$(TESTS))

installcheck-mock: install
    $(MAKE) installcheck REGRESS="$(REGRESS) llm_mock health_monitor"
```

### mock 서버 (examples/mock_openai.py)

`BaseHTTPRequestHandler`는 `do_GET` 없으면 자동으로 501 반환.
health check용 GET을 처리하려면 `do_GET` 명시 필수.

```python
def do_GET(self):
    body = b'{}'
    self.send_response(200)
    self.send_header("Content-Type", "application/json")
    self.send_header("Content-Length", str(len(body)))
    self.end_headers()
    self.wfile.write(body)
```

### expected 파일 생성 패턴

pg_regress 출력에 trailing whitespace가 있을 수 있다.
실제 출력 파일로 expected를 갱신:

```bash
make installcheck; cp results/foo.out test/expected/foo.out
# trailing whitespace 확인
python3 -c "
with open('test/expected/foo.out') as f:
    for i,l in enumerate(f,1):
        if l.rstrip() != l.rstrip('\n'):
            print(i, repr(l))
"
```

---

## 6. Docker 빌드 환경

### 참조 파일
- `examples/rust/Dockerfile.dev`
- `examples/c/docker-compose.yml`

### 기반 이미지 통일 (GLIBC 버전)

`postgres:17` (Trixie, GLIBC 2.38)과 `pgvector/pgvector:pg17` (Bookworm, GLIBC 2.36)은 다르다.
builder와 pg 컨테이너를 다른 이미지로 쓰면 `.so` 로드 실패.

pg_aidb는 pgvector가 필요하므로 **둘 다 `pgvector/pgvector:pg17` 기반으로 통일**.

### pgvector 수동 설치 시 root 권한 필요

```dockerfile
# Dockerfile.dev (examples/rust/Dockerfile.dev:13~19 참조)
# pgvector는 반드시 USER dev 이전에 root로 설치
RUN git clone --depth 1 --branch v0.8.0 https://github.com/pgvector/pgvector.git /tmp/pgvector \
    && cd /tmp/pgvector \
    && make PG_CONFIG=/usr/bin/pg_config \
    && make install PG_CONFIG=/usr/bin/pg_config \
    && rm -rf /tmp/pgvector

USER dev
```

`make install`이 `/usr/include/postgresql/17/server/` 에 쓰므로 root 필요.

### builder 서비스에 command 명시

`pgvector/pgvector:pg17` 기반 이미지의 기본 CMD는 `postgres` 서버 시작.
`docker compose run --rm builder` 시 command 없으면 postgres 시작 시도 → 에러.

```yaml
# docker-compose.yml (examples/c/docker-compose.yml:29 참조)
builder:
  command: bash -c "make -C extension install && cargo pgrx test"
```

### 이미지 교체 후 make clean 필수

호스트 마운트 볼륨에 이전 `.o` 파일이 남아있으면
새 기반 이미지로 빌드해도 stale 오브젝트가 재사용된다.

```bash
make clean && make
```

### cargo pgrx init 패턴

```dockerfile
# examples/rust/Dockerfile.dev:38
RUN cargo pgrx init --pg17 /usr/bin/pg_config
```

`/usr/bin/pg_config`는 시스템 설치 경로. `.pgrx/` 안에 별도 pg_config 없음.

---

## 7. SQL 스키마 패턴

### 참조 파일
- `examples/c/test/sql/test.sql` (전체)
- `examples/c/test/test/sql/` (regress 쿼리)

### pg_extension_config_dump 필수

사용자 데이터가 있는 테이블에 빠뜨리면 `pg_dump`가 데이터를 출력하지 않는다.

```sql
-- ai.models, ai.endpoints 등 모든 사용자 데이터 테이블에
SELECT pg_extension_config_dump('ai.models', '');
SELECT pg_extension_config_dump('ai.endpoints', 'WHERE NOT is_system_default');
```

### RLS + owner 패턴 (examples/c/test/sql/test.sql:76~77)

```sql
ALTER TABLE ai.credentials ENABLE ROW LEVEL SECURITY;
CREATE POLICY cred_owner ON ai.credentials USING (owner = current_user);
```

---

## 8. ai.results 테이블 (비동기 모드 기반)

ADR-006에서 결정한 NOTIFY-as-hint + table-as-truth 패턴:

```sql
CREATE TABLE ai.results (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    request_id  uuid NOT NULL,
    status      text NOT NULL DEFAULT 'pending',  -- pending|running|done|error
    data        jsonb,
    error_msg   text,
    created_at  timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz
);

-- 완료 시
INSERT INTO ai.results(request_id, status, data, finished_at)
    VALUES ($1, 'done', $2, now());
PERFORM pg_notify('ai_' || $1::text, $1::text);
```

클라이언트는 NOTIFY 또는 폴링 중 자유 선택. 강제 없음.

---

## 9. 설계 결정 요약

모든 ADR은 `design/DECISIONS.md`에 있다. 핵심만:

| ADR | 결정 | 핵심 이유 |
|-----|------|----------|
| 001 | BGW → 외부 pipeline-worker | standalone 미지원, 스케일 아웃 필요 |
| 002 | Extension (.so) 유지 | GUC, hook, pg_cuvs 연동 |
| 003 | RAG는 외부 플랫폼 책임 | Extension crash = PG crash |
| 004 | 3층 스키마 (platform/platform_ai/ai) | SQL 가시성 vs 결합도 타협 |
| 005 | Skill-Plan-Execute 공통 패턴 | 지터 완화, 캐싱 |
| 006 | 이중 모드 API + NOTIFY-as-hint | pg_net은 동기 블로킹 해결 안 함 |

---

## 10. 구현 시작 순서 권장

1. **extension skeleton**: `cargo pgrx new` → Cargo.toml + pg_aidb.control 설정
2. **ai 스키마 + model_registry**: `extension_sql!`로 테이블 생성, `requires` 순서 주의
3. **ai.results 테이블**: 비동기 모드 기반 인프라
4. **동기 HTTP 클라이언트**: reqwest blocking, endpoint 조회 → HTTP POST
5. **ai.embed() 동기**: credentials 조회(SPI) → HTTP → float4[] 반환
6. **ai.embed_async()**: `ai.results`에 `status='pending'` 행 INSERT → `pg_notify('ai_' || request_id::text, request_id::text)` 힌트 발행 → UUID 즉시 반환. ADR-006에 따라 **pg_net 미사용**. 클라이언트는 `LISTEN "ai_<uuid>"` 또는 `ai.results` 폴링 중 선택.
7. **Docker Compose**: pg + builder + mock 서비스
8. **regression test**: mock 포함/제외 두 타겟

---

## 11. 함정 체크리스트

pg_aidb 코드 작성 전 확인:

- [ ] `pg_aidb.control`에 `requires = 'vector'` 없음
- [ ] pgvector 타입 쓰는 SECURITY DEFINER 함수에 `public` in search_path
- [ ] `extension_sql!`의 `requires = [...]`에 의존 식별자 모두 포함
- [ ] builder Dockerfile에 pgvector 설치를 `USER dev` 이전에
- [ ] docker-compose builder 서비스에 `command:` 명시
- [ ] 외부 API 의존 테스트를 MOCK_TESTS로 분리
- [ ] mock 서버에 `do_GET` 구현 (health check용)
- [ ] 모든 사용자 테이블에 `pg_extension_config_dump()` 호출
- [ ] 이중 모드 함수 시그니처: `ai.foo()` (동기) + `ai.foo_async()` (비동기)
