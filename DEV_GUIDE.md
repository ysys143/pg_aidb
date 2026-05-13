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

## 참고

- [PostgreSQL Extension 공식 문서](https://www.postgresql.org/docs/current/extend-extensions.html)
- [PGXS 빌드 시스템](https://www.postgresql.org/docs/current/extend-pgxs.html)
- [pgrx GitHub](https://github.com/pgcentralfoundation/pgrx)
