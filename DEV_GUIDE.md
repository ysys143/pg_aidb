# Mac에서 Linux용 PostgreSQL Extension 개발 가이드

## 개요

Mac에서 코드를 작성하고, Linux 환경에서 빌드/테스트하여 Linux 서버에 배포하는 워크플로우를 다룹니다.

---

## 왜 Linux 환경이 필요한가

PostgreSQL extension 빌드는 `pg_config`가 반환하는 경로와 플래그에 의존합니다.  
Mac의 `pg_config`와 Linux의 `pg_config`는 컴파일러/링커 플래그가 다르기 때문에,  
Linux 타겟 배포라면 반드시 Linux 환경에서 빌드해야 합니다.

---

## 접근 방식

### 1. Docker (권장)

가장 일반적인 방법입니다. Mac에서 편집하고 Docker 컨테이너에서 빌드합니다.

#### Dockerfile

```dockerfile
FROM postgres:16
RUN apt-get update && apt-get install -y \
    build-essential \
    postgresql-server-dev-16
```

#### 빌드 명령

```bash
# 이미지 빌드
docker build -t pg-ext-dev .

# 컨테이너에서 빌드 + 설치
docker run --rm -v $(pwd):/ext -w /ext pg-ext-dev \
    bash -c "make && make install"

# 대화형 셸 접속
docker run --rm -it -v $(pwd):/ext -w /ext pg-ext-dev bash
```

#### docker-compose.yml (개발용)

```yaml
version: "3.9"
services:
  db:
    image: postgres:16
    environment:
      POSTGRES_PASSWORD: postgres
    volumes:
      - .:/ext
    ports:
      - "5432:5432"

  builder:
    build: .
    volumes:
      - .:/ext
    working_dir: /ext
    command: bash -c "make && make install && pg_ctl start"
```

---

### 2. pgrx (Rust 기반 extension)

Rust로 extension을 작성할 때의 표준 도구입니다.

```bash
# 설치
cargo install cargo-pgrx

# 초기화 (여러 PG 버전 로컬 설치)
cargo pgrx init

# 새 프로젝트 생성
cargo pgrx new my_extension

# 개발 서버 실행
cargo pgrx run

# 테스트
cargo pgrx test

# 배포용 패키지 생성
cargo pgrx package
```

---

### 3. C Extension (전통적인 방법)

#### 프로젝트 구조

```
my_extension/
├── Makefile
├── my_extension.c
├── my_extension.control
└── my_extension--1.0.sql
```

#### Makefile

```makefile
MODULES = my_extension
EXTENSION = my_extension
DATA = my_extension--1.0.sql

PG_CONFIG = pg_config
PGXS := $(shell $(PG_CONFIG) --pgxs)
include $(PGXS)
```

#### my_extension.control

```
comment = 'My PostgreSQL Extension'
default_version = '1.0'
module_pathname = '$libdir/my_extension'
relocatable = true
```

#### my_extension.c (최소 예시)

```c
#include "postgres.h"
#include "fmgr.h"

PG_MODULE_MAGIC;

PG_FUNCTION_INFO_V1(my_function);

Datum
my_function(PG_FUNCTION_ARGS)
{
    PG_RETURN_TEXT_P(cstring_to_text("Hello from extension!"));
}
```

#### my_extension--1.0.sql

```sql
CREATE FUNCTION my_function()
RETURNS text
AS '$libdir/my_extension'
LANGUAGE C STRICT;
```

---

## 빌드 및 테스트

```bash
# 빌드
make

# 설치 (PostgreSQL lib 디렉토리로 복사)
make install

# PostgreSQL에서 extension 로드
psql -c "CREATE EXTENSION my_extension;"

# 테스트
psql -c "SELECT my_function();"

# 삭제
make uninstall
```

---

## 개발 워크플로우

```
Mac (VS Code 편집)
       |
       v
Docker / Lima (Linux 빌드 + 테스트)
       |
       v
GitHub Actions (CI - ubuntu-latest)
       |
       v
Linux 서버 배포 (.so + .control + .sql)
```

---

## CI: GitHub Actions 예시

```yaml
name: Build and Test

on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest
    services:
      postgres:
        image: postgres:16
        env:
          POSTGRES_PASSWORD: postgres
        options: >-
          --health-cmd pg_isready
          --health-interval 10s
          --health-timeout 5s
          --health-retries 5

    steps:
      - uses: actions/checkout@v4

      - name: Install dependencies
        run: |
          sudo apt-get update
          sudo apt-get install -y postgresql-server-dev-16

      - name: Build
        run: make

      - name: Install
        run: sudo make install

      - name: Test
        run: make installcheck
        env:
          PGPASSWORD: postgres
```

---

## Mac 로컬 개발 도구

| 도구 | 용도 |
|------|------|
| VS Code + clangd | C/C++ 코드 편집 및 자동완성 |
| Docker Desktop | Linux 빌드 환경 |
| Lima | 경량 Linux VM (Docker 대안) |
| pgAdmin / TablePlus | DB GUI 클라이언트 |
| `pg_format` | SQL 포맷터 |

### clangd 설정 (compile_commands.json 생성)

```bash
bear -- make
```

---

## 배포 파일 목록

```
/usr/lib/postgresql/16/lib/my_extension.so   # 공유 라이브러리
/usr/share/postgresql/16/extension/
    my_extension.control                      # 메타데이터
    my_extension--1.0.sql                     # SQL 정의
```

---

## 언어 선택: C vs Rust vs Python

### 한눈에 비교

| 항목 | C | Rust | Python (PL/Python) |
|------|---|------|--------------------|
| 성능 | 최고 | C와 동급 | 낮음 (인터프리터) |
| 안전성 | 낮음 (직접 메모리 관리) | 높음 (컴파일 타임 보장) | 높음 |
| 개발 속도 | 느림 | 중간 | 빠름 |
| 빌드 복잡도 | 중간 (PGXS) | 낮음 (cargo-pgrx) | 없음 (설치 즉시 사용) |
| PostgreSQL 내부 접근 | 완전 | 완전 | 제한적 |
| 배포 | .so 파일 | .so 파일 | SQL 파일만 |
| 학습 난이도 | 높음 | 중간-높음 | 낮음 |
| 생태계 | PostgreSQL 표준 | 성장 중 | Python 전체 활용 가능 |

---

### C

PostgreSQL 자체가 C로 작성되어 있어, 내부 API를 가장 직접적으로 사용할 수 있습니다.

**장점:**
- PostgreSQL 내부 구조에 완전 접근 (Planner hook, Executor hook 등)
- 최고 성능
- 모든 PostgreSQL 버전 지원

**단점:**
- 메모리 오류(segfault, buffer overflow) 위험
- `palloc` / `pfree` 등 PostgreSQL 메모리 컨텍스트를 직접 관리해야 함
- 디버깅 어려움

**적합한 경우:**
- Custom index access method
- Background worker
- Planner/Executor hook
- 기여하거나 fork할 PostgreSQL 핵심 기능

```c
#include "postgres.h"
#include "fmgr.h"
#include "utils/builtins.h"

PG_MODULE_MAGIC;

PG_FUNCTION_INFO_V1(add_two);

Datum
add_two(PG_FUNCTION_ARGS)
{
    int32 a = PG_GETARG_INT32(0);
    int32 b = PG_GETARG_INT32(1);
    PG_RETURN_INT32(a + b);
}
```

---

### Rust (pgrx)

현대적인 extension 개발의 표준으로 자리잡고 있습니다.

**장점:**
- 컴파일 타임 메모리 안전성 (C의 주요 위험 제거)
- `cargo-pgrx`로 개발 경험 우수 (`cargo pgrx run` 한 줄로 실행)
- Rust 생태계(crates.io) 활용 가능
- 타입 안전한 PostgreSQL API 바인딩

**단점:**
- Rust 언어 자체의 학습 곡선
- pgrx 버전과 PostgreSQL 버전 호환성 관리 필요
- 빌드 시간이 C보다 김

**적합한 경우:**
- 새로운 extension 프로젝트 시작 시
- 복잡한 비즈니스 로직이 포함된 함수
- 외부 Rust 라이브러리를 PostgreSQL 내부에서 사용할 때

```rust
use pgrx::prelude::*;

pg_module_magic!();

#[pg_extern]
fn add_two(a: i32, b: i32) -> i32 {
    a + b
}

#[cfg(any(test, feature = "pg_test"))]
#[pg_schema]
mod tests {
    use pgrx::prelude::*;

    #[pg_test]
    fn test_add_two() {
        assert_eq!(6, crate::add_two(2, 4));
    }
}
```

---

### Python (PL/Python3u)

PostgreSQL에 내장된 절차적 언어(Procedural Language)로, 별도 빌드 없이 사용합니다.

**장점:**
- 빌드 없음 - SQL 파일만으로 배포
- Python 라이브러리(numpy, pandas, scikit-learn 등) 직접 사용 가능
- 빠른 프로토타이핑
- 데이터 과학/ML 워크로드에 적합

**단점:**
- 성능이 C/Rust 대비 크게 낮음
- `u` (untrusted) 접미사 - 슈퍼유저 권한 필요
- PostgreSQL 내부 API 접근 불가
- Python 환경(버전, 패키지) 서버에 사전 설치 필요

**적합한 경우:**
- 데이터 변환, 집계, ML 추론 함수
- 빠른 프로토타이핑 후 C/Rust로 재작성 예정인 경우
- Python 라이브러리가 핵심인 기능 (예: 텍스트 분석, 통계)

```sql
CREATE EXTENSION plpython3u;

CREATE FUNCTION add_two(a integer, b integer)
RETURNS integer
AS $$
    return a + b
$$ LANGUAGE plpython3u;

-- numpy 활용 예시
CREATE FUNCTION array_mean(arr float8[])
RETURNS float8
AS $$
    import numpy as np
    return float(np.mean(arr))
$$ LANGUAGE plpython3u;
```

---

### 언어 선택 기준

```
성능이 최우선이고 PostgreSQL 내부를 다뤄야 한다
    → C

새 프로젝트이고 안전성 + 생산성을 원한다
    → Rust (pgrx)

ML/데이터 처리이거나 빠른 프로토타이핑이 목적이다
    → Python (PL/Python3u)

Python으로 검증 후 프로덕션 성능이 필요해졌다
    → Python → Rust로 재작성
```

---

## 참고

- [PostgreSQL Extension 공식 문서](https://www.postgresql.org/docs/current/extend-extensions.html)
- [PGXS 빌드 시스템](https://www.postgresql.org/docs/current/extend-pgxs.html)
- [pgrx GitHub](https://github.com/pgcentralfoundation/pgrx)
