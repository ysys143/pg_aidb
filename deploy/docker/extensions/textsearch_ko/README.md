textsearch_ko
=============

PostgreSQL extension module for Korean full text search using mecab

PostgreSQL 데이터베이스 서버에서 사용할 한글 형태소 분석기 기반 전문 검색 모듈 소스입니다.

> **이 버전은 [i0seph/textsearch_ko](https://github.com/i0seph/textsearch_ko) 포크입니다.**
> BM25 랭킹(pg_textsearch, pl/pgsql bm25_ranking)과의 조합에서 recall을 높이기 위한
> 토크나이저 개선이 적용되어 있습니다.

---

## 원본 대비 개선사항 (Enhanced tokenizer)

### 1. OOV surface passthrough
mecab-ko 사전에 없는 한글 토큰(미등록어)을 버리지 않고 surface form 그대로 색인합니다.
신조어, 기술 약어, 고유명사가 보존됩니다.

### 2. VV+하다 dual indexing
기본형이 `하다`로 끝나는 동사(VV)에 대해 전체 기본형(`모니터링하다`)과
어근(`모니터링`)을 동시에 색인합니다.
명사 쿼리가 `하다` 동사 문서에 히트할 수 있습니다.

### 3. NNG 복합명사 분해
mecab-ko-dic의 복합명사 분해 정보(`정보/NNG+검색/NNG`)를 파싱하여
복합체 기본형과 각 구성 명사를 동일 tsvector position에 동시 색인합니다.
부분 용어 recall이 향상됩니다.

### 4. SL(외래어) 태그 추가
영문/외래어 토큰(`SL` 품사 태그)을 색인 대상에 포함합니다.
한국어 기술 문서에 섞인 영문 용어가 보존됩니다.

### 벤치마크 (MIRACL-ko 10k + EZIS 97 docs)

pg_textsearch BM25/WAND(`<@>` 연산자) 기준:

| 데이터셋 | 원본 NDCG@10 | Enhanced NDCG@10 | 향상 | Latency p50 |
|---------|------------|----------------|------|-------------|
| MIRACL-ko | 0.1815 | 0.3374 | +86% | ~0.86ms |
| EZIS | 0.0076 | 0.8417 | ×110 | — |

> **주의**: enhanced tokenizer는 **BM25 랭킹과 함께 사용**해야 합니다.
> `ts_rank_cd`(AND 필터)와 조합하면 세분화된 토큰 때문에 recall이 급감합니다.
> pg_textsearch의 `<@>` 연산자 또는 pl/pgsql `bm25_ranking()` 함수와 사용하세요.

---

# 설치방법

  
## 1. mecab-ko 설치
https://bitbucket.org/eunjeon/mecab-ko

페이지를 참조

aarch64 환경에서는 configure 작업할 때, --build=aarch64-unknown-linux-gnu 옵션 추가해야함. (워낙 오래된 라이브러리라서)
## 2. mecab-ko-dic 설치
https://bitbucket.org/eunjeon/mecab-ko-dic

페이지를 참조
## 3. textsearch_ko 설치
데이터베이스 인코딩은 반드시 utf-8이어야함!
```
export PATH=/opt/mecab-ko/bin:/postgres/15/bin:$PATH
make USE_PGXS=1 install
```
.so 파일의 mecab-ko 라이브러리 rpath 설정하는 방법 모름. 알아서 잘.
## 4. 테스트
```
ioseph@localhost:~/textsearch_ko$ psql
 Pager usage is off.
 psql (9.3.5)
 Type "help" for help.
 
 ioseph=# CREATE EXTENSION textsearch_ko;

 ioseph=# -- 기본 언어를 한국어로 설정
          set default_text_search_config = korean;
 SET
 ioseph=# -- mecab-ko 모듈이 정상 작동하는지 확인
          select * from mecabko_analyze('무궁화꽃이 피었습니다.');
  word  | type | part1st | partlast | pronounce | conjtype | conjugation | basic | detail  |                      lucene
 --------+------+---------+----------+-----------+----------+-------------+-------+---------+---------------------------------------------------
 무궁화 | NNG  |         | F        | 무궁화    | Compound |             |       | 무궁+화 | 무궁/NNG/*/1/1+무궁화/Compound/*/0/2+화/NNG/*/1/1
 꽃     | NNG  |         | T        | 꽃        |          |             |       |         |
 이     | JKS  |         | F        | 이        |          |             |       |         |
 피     | VV   |         | F        | 피        |          |             |       |         |
 었     | EP   |         | T        | 었        |          |             |       |         |
 습니다 | EF   |         | F        | 습니다    |          |             |       |         |
 .      | SF   |         |          | .         |          |             |       |         |
 (7 rows)
 
 ioseph=#  -- 필요없는 조사, 어미들을 빼고 백터로 만드는지 확인 
         select * from to_tsvector('무궁화꽃이 피었습니다.');
       to_tsvector
 --------------------------
 '꽃':2 '무궁화':1 '피':3
 (1 row)
 
  ioseph=# select * from to_tsvector('그래서, 무궁화꽃이 피겠는걸요?');
       to_tsvector
 --------------------------
  '꽃':2 '무궁화':1 '피':3
 (1 row)
```
