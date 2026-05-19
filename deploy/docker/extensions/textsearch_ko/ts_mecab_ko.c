/*
 * ts_mecab_ko.c
 * License : BSD
 * This file is modified from textsearch_ja.c 
 * Change Contents
 *  - code refactoring
 *  - customizing for mecab-ko-dic data
 * Copyright (c) 2014, Ioseph Kim
 */
#include "postgres.h"

#include "access/heapam.h"
#include "access/htup_details.h"
#include "catalog/pg_type.h"
#include "funcapi.h"
#include "mb/pg_wchar.h"
#include "tsearch/ts_public.h"
#include "tsearch/ts_utils.h"
#include "utils/builtins.h"

#include "ts_mecab_ko.h"
#include <mecab.h>

PG_MODULE_MAGIC;

/*
 * 이 파서가 분석 가능한 노드 형태들
 */
#define WORD_T			2	/* Word, all letters */
#define NUMWORD			3	/* Word, letters and digits */
#define NUMPARTHWORD	9	/* Hyphenated word part, letters and digits */
#define PARTHWORD		10	/* Hyphenated word part, all letters */
#define NUMHWORD		15	/* Hyphenated word, letters and digits */
#define HWORD			17	/* Hyphenated word, all letters */

/*
# define IS_MECAB_WORD(t)	( \
	(t) == WORD_T || (t) == NUMWORD || (t) == NUMPARTHWORD || \
	(t) == PARTHWORD || (t) == NUMHWORD || (t) == HWORD)
*/

# define IS_MECAB_WORD(t)	( \
	(t) == WORD_T || (t) == NUMPARTHWORD || \
	(t) == PARTHWORD || (t) == NUMHWORD || (t) == HWORD)

#define SPACE			12

/* MeCab 에서 넘겨준 CSV 값들 (mecab-ko, mecab-ko-dic 자료기준) */
#define NUM_CSV			9
#define MECAB_BASIC		3	/* 기본형 */
#define MECAB_CONJTYPE		4	/* 용언활용 */
#define MECAB_DETAIL		7	/* 활용정보 */

#define SEPARATOR_CHAR	'\v'

/* UTF-8 인코딩 '하다' (U+D558 U+B2E4): 6 bytes */
#define HADA_BYTES		6

/*
 * parser_data - 파싱 작업 중인 자료
 */
typedef struct parser_data
{
	StringInfoData		str;
	const mecab_node_t	*node;		/* mecab-ko 분석기에서 넘겨준 노드 */
	Datum			ascprs;		/* ascii word parser */
	const char		*last_node_pos;
} parser_data;

PG_FUNCTION_INFO_V1(ts_mecabko_start);
PG_FUNCTION_INFO_V1(ts_mecabko_gettoken);
PG_FUNCTION_INFO_V1(ts_mecabko_end);
PG_FUNCTION_INFO_V1(ts_mecabko_lexize);
PG_FUNCTION_INFO_V1(mecabko_analyze);
PG_FUNCTION_INFO_V1(korean_normalize);
PG_FUNCTION_INFO_V1(hanja2hangul);

extern void PGDLLEXPORT _PG_init(void);
extern void PGDLLEXPORT _PG_fini(void);
extern Datum PGDLLEXPORT ts_mecabko_start(PG_FUNCTION_ARGS);
extern Datum PGDLLEXPORT ts_mecabko_gettoken(PG_FUNCTION_ARGS);
extern Datum PGDLLEXPORT ts_mecabko_end(PG_FUNCTION_ARGS);
extern Datum PGDLLEXPORT ts_mecabko_lexize(PG_FUNCTION_ARGS);
extern Datum PGDLLEXPORT mecabko_analyze(PG_FUNCTION_ARGS);
extern Datum PGDLLEXPORT korean_normalize(PG_FUNCTION_ARGS);
extern Datum PGDLLEXPORT hanja2hangul(PG_FUNCTION_ARGS);

static bool	feature(const mecab_node_t *node, int n, const char **t, int *tlen);
static void	normalize(StringInfo dst, const char *src, size_t srclen, append_t append);
static char	*lexize(const char *str, size_t len);
static bool	accept_mecab_ko_part(const char *str, int slen);
static void	appendString(StringInfo dst, const unsigned char *src, int srclen);
static bool	ismbascii(const unsigned char *s, unsigned char *c, int *cnt);
/* 2-C 개선: OOV / 하다 / 복합명사 */
static bool	has_hangul(const char *s, int len);
static bool	ends_with_hada(const char *s, int slen);
static TSLexeme *compound_lexemes(const char *base, int baselen,
								  const char *compound, int complen);

/* mecab-ko-dic 에서 사용할 품사들 */
/* 2-C: SL(외래어) 추가 — 기술 문서의 영문/외래어 태그 포함 */
static char *accept_parts_of_speech[14] = {
	"NNG" ,"NNP" ,"NNB" ,"NNBC" ,"NR" ,"VV" ,"VA" ,"MM" ,"MAG" ,"XSN" ,"XR" ,"SH" ,"SL", ""
};

static char *ascii_sign = "`~!@#$%^&*()-=\\_+|[]{};':\",.<>/? ";

/* mecab */
static mecab_t	   *_mecab;

/*
 * mecab_assert - mecab 오류 처리
 */
#define mecab_assert(expr) \
	if (expr); else \
		ereport(ERROR, \
			(errcode(ERRCODE_EXTERNAL_ROUTINE_EXCEPTION), \
			 errmsg("mecab: %s", mecab_strerror(_mecab))))

/*
 * mecab_acquire - 사전 인코딩과 DB 인코딩이 다르면 종료
 */

static int	mecab_dict_encoding = -1;

static mecab_t *
mecab_acquire(void)
{
	if (mecab_dict_encoding < 0)
	{
		const mecab_dictionary_info_t *dict = mecab_dictionary_info(_mecab);
		int		encoding = pg_char_to_encoding(dict->charset);

		if (encoding != GetDatabaseEncoding())
		{
			ereport(ERROR,
				(errcode(ERRCODE_EXTERNAL_ROUTINE_EXCEPTION),
				errmsg("mecab: encoding mismatch (db=%s, mecab=%s)",
					GetDatabaseEncodingName(), dict->charset)));
		}

		mecab_dict_encoding = encoding;
	}

	return _mecab;
}

/*
 * _PG_init - 동적 모듈 초기화
 */
void
_PG_init(void)
{
	if (_mecab == NULL)
	{
		int			argc = 1;
		char	   *argv[] = { "mecab" };
		_mecab = mecab_new(argc, argv);
		mecab_assert(_mecab);
	}
}

/*
 * _PG_fini - 뒷 정리
 */
void
_PG_fini(void)
{
	if (_mecab != NULL)
	{
		mecab_destroy(_mecab);
		_mecab = NULL;
	}
}

/*
 * ts_mecabko_start - 파서 시작 함수
 * mecab_sparse_tonode2 호출
 * 영어 쪽을 위해 prsd_start 도 같이 호출
 */
Datum
ts_mecabko_start(PG_FUNCTION_ARGS)
{
	mecab_t			   *mecab = mecab_acquire();
	char			   *input = (char *) PG_GETARG_POINTER(0);
	int					len	= PG_GETARG_INT32(1);
	parser_data	   *parser;

	parser = (parser_data *) palloc(sizeof(parser_data));
	initStringInfo(&parser->str);
	/*
	 * XXX: 한국어 문자열 일반화
         * 전각 영숫자는 소문자로
         * 한자는 한글로 (못하면 그대로)
	 */
	normalize(&parser->str, input, len, appendString);

	/* Replace input text to the normalized text. */
	input = parser->str.data;
	len = parser->str.len;

	/*
	 * 파싱
	 */
	parser->node = mecab_sparse_tonode2(mecab, input, len);
	mecab_assert(parser->node);

	/* 영숫자는 prsd 쪽으로 넘김 */
	parser->ascprs = DirectFunctionCall2(
		prsd_start, CStringGetDatum(input), Int32GetDatum(len));
	parser->last_node_pos = NULL;

    PG_RETURN_POINTER(parser);
}

static const mecab_node_t *
next_token(parser_data *parser)
{
	const mecab_node_t *result;

	for (; parser->node != NULL; parser->node = parser->node->next)
	{
		/* 파서 자료 중 처음과 끝 무시 */
		switch (parser->node->stat)
		{
		case MECAB_BOS_NODE:
		case MECAB_EOS_NODE:
			continue;
		}

		result = parser->node;
		parser->node = parser->node->next;
		return result;
	}

	return NULL;	/* 末尾 */
}

/*
 * FIXME: グローバル変数 current_node 経由で処理中の node を渡すのは非常に危険
 * なのだが、他に ts_headline に対応する方法が無いので仕方なくこの方法を取っている.
 * この方式だと、ts_debug() が期待通りに動作しない問題がある.
 * FIXME : 전역 변수 current_node 통해 처리되는 node를 전달하는 것은 매우 위험하지만,
 * 다른 ts_headline에 대응하는 방법이 없기 때문에 어쩔 수없이 이 방법을 취하고있다.
 * 이 방식때문에 ts_debug ()이 예상대로 작동하지 않는 문제가있다. (구글번역)
 * 사전 처리에 문제가 있음 - ioseph
 */

static const mecab_node_t *current_node;

Datum
ts_mecabko_gettoken(PG_FUNCTION_ARGS)
{
	parser_data	*parser = (parser_data *) PG_GETARG_POINTER(0);
	const char	**t = (const char **) PG_GETARG_POINTER(1);
	int		*tlen  = (int *) PG_GETARG_POINTER(2);
	int		lextype;
	const char	*skip;
	const mecab_node_t *node;
	const char	*conjstr;
	int		conjlen;

	current_node = NULL;

	if (parser->last_node_pos == NULL)
	{
		for (;;)
		{
			/* 일단 기본 파서로 노드 형식을 구함 */
			lextype = DatumGetInt32(DirectFunctionCall3(
				prsd_nexttoken, parser->ascprs,
				PointerGetDatum(t), PointerGetDatum(tlen)));

			if (lextype == 0)
			{
				/* 파싱 완료 */
				PG_RETURN_INT32(0);
			}
			else if (lextype == SPACE && *tlen > 0 && **t == SEPARATOR_CHAR)
			{
				/* \v 문자인데, space면 무시 */
				continue;
			}
			else if (IS_MECAB_WORD(lextype))
			{
				/* 파싱 작업 대상이 됨 */
				skip = *t;
				parser->last_node_pos = *t + *tlen;
				break;
			}
			else
			{
				/* 그밖은 그대로 통과 */
				parser->last_node_pos = NULL;
				PG_RETURN_INT32(lextype);
			}
		}
	}
	else
		skip = NULL;

	do
	{
		node = next_token(parser);
		if (node == NULL)
			PG_RETURN_INT32(0);
	} while (node->surface < skip);

	/* 검색에 사용할 품사만 거르고 나머지는 통과 */
	if ((feature(node, MECAB_CONJTYPE, &conjstr, &conjlen))
                   && (strncmp(conjstr, "Inflect,", 8) == 0)
                   && (feature(node, MECAB_DETAIL, &conjstr, &conjlen))){
		lextype = WORD_T;
	}
	else if (accept_mecab_ko_part(node->feature, strchr(node->feature, ',') - node->feature))
		lextype = WORD_T;
	/* 2-C: OOV 한국어 미등록어 — 사전에 없는 한글 포함 토큰은 surface 그대로 통과
	 * lexize()에서 별도 처리 없이 surface form 인덱싱 (기술 약어, 신조어, 고유명사 보존) */
	else if (node->stat == MECAB_UNK_NODE && has_hangul(node->surface, node->length))
		lextype = WORD_T;
	else
		lextype = SPACE;

	*t = node->surface;
	*tlen = node->length;
	if (*t + *tlen >= parser->last_node_pos)
		parser->last_node_pos = NULL;

	current_node = node;

	PG_RETURN_INT32(lextype);
}

/*
 * ts_mecabko_end - 파싱 뒷작업
 */
Datum
ts_mecabko_end(PG_FUNCTION_ARGS)
{
	parser_data *parser = (parser_data *) PG_GETARG_POINTER(0);

	current_node = NULL;

	DirectFunctionCall1(prsd_end, parser->ascprs);

	pfree(parser->str.data);
	pfree(parser);

	PG_RETURN_VOID();
}


/*
 * ts_mecabko_lexize - 사전처리
 * 현재 ts_lexize 에서 의도된 대로 움직이지 않음
 * to_tsvector 에서만 의도된 대로 됨
 * 위에서 말한 current_node 전역변수 처리 때문
 *
 * 이 함수로 넘어오는 단어는 형태소 분석기가 의미 단위로 분리한 그 단어가 넘어온다.
 * 예를 들어 '가까워졌음을'이 입력되면, {가깝,어,지,었,음,을} 로 분리하거나,
 * 약어, 동의어, 자동수정 등 기능을 할 수 있다.
 * 
 * 현재는 단지 용언 활용 부분만 처리한다.
 */
Datum
ts_mecabko_lexize(PG_FUNCTION_ARGS)
{

	const char *t = (char *) PG_GETARG_POINTER(1);
	int			tlen = PG_GETARG_INT32(2);
	TSLexeme   *res;
	int pluscnt = 0;
	const char *pluspos;
	const char *slashpos;
	int i;

	if (current_node) {
		/* 2-C: OOV 미등록어 — lexize 사전 처리 없이 surface form 그대로 반환 */
		if (current_node->stat == MECAB_UNK_NODE)
		{
			res = palloc0(sizeof(TSLexeme) * 2);
			res[0].lexeme = lexize(t, tlen);
		}
		else if ((feature(current_node, MECAB_CONJTYPE, &t, &tlen))
		   && (strncmp(t, "Inflect,", 8) == 0)
		   && (feature(current_node, MECAB_DETAIL, &t, &tlen))){
			do {
				pluspos = strchr(t, '+');
				pluscnt += 1;
				if(pluspos != NULL) t = pluspos + 1;
			} while (pluspos != NULL);
			feature(current_node, MECAB_DETAIL, &t, &tlen);
			res = palloc0(sizeof(TSLexeme) * (pluscnt + 1));
			i = 0;
			do {
				pluspos = strchr(t, '+');
				slashpos = strchr(t, '/');
				/* accept_mecab_ko_part 호출해서 제외 품사면 통과 */
				if(accept_mecab_ko_part(slashpos + 1, strchr(slashpos + 1, '/') - slashpos - 1)){
					res[i].lexeme = lexize(t, slashpos - t);
					i += 1;
				}
				if(pluspos != NULL)
					t = pluspos + 1;
			} while (pluspos != NULL);
		}
		else {
			const char *base = NULL;
			int			baselen = 0;
			const char *pos_str = NULL;
			int			pos_len = 0;
			const char *compound;
			int			complen;

			feature(current_node, MECAB_BASIC, &base, &baselen);
			feature(current_node, 0, &pos_str, &pos_len);

			/* 2-C: VV + "하다" 어미 → 기본형("모니터링하다") + NNG 어근("모니터링") 동시 인덱싱
			 * 한국어 기술문서의 "명사+하다" 패턴: 쿼리 "모니터링"이 "모니터링하다" 문서에 히트 */
			if (pos_str && pos_len == 2 && strncmp(pos_str, "VV", 2) == 0 &&
				ends_with_hada(base, baselen))
			{
				res = palloc0(sizeof(TSLexeme) * 3);
				res[0].lexeme = lexize(base, baselen);
				res[1].lexeme = lexize(base, baselen - HADA_BYTES);
			}
			/* 2-C: NNG 복합명사 분해 — mecab-ko-dic field4 "정보/NNG+검색/NNG" 파싱
			 * 복합체와 구성 명사 동시 인덱싱 → partial-term recall 향상 */
			else if (pos_str && pos_len == 3 && strncmp(pos_str, "NNG", 3) == 0 &&
					 feature(current_node, MECAB_CONJTYPE, &compound, &complen) &&
					 memchr(compound, '/', complen) != NULL)
			{
				res = compound_lexemes(base, baselen, compound, complen);
			}
			else
			{
				res = palloc0(sizeof(TSLexeme) * 2);
				res[0].lexeme = lexize(base, baselen);
			}
		}
	}
	else {
		res = palloc0(sizeof(TSLexeme) * 2);
		res[0].lexeme = lexize(t,tlen);
	}

	PG_RETURN_POINTER(res);
}

#define make_text(s, ln) \
	PointerGetDatum(cstring_to_text_with_len((s), (ln)))

/*
 * mecabko_analyze - mecab node dump
 */
Datum
mecabko_analyze(PG_FUNCTION_ARGS)
{
	mecab_t		   *mecab = mecab_acquire();
	FuncCallContext	   *funcctx;
	List		   *tuples;
	HeapTuple	tuple;
	HeapTuple	result;

	if (SRF_IS_FIRSTCALL())
	{
		text		   *txt = PG_GETARG_TEXT_PP(0);
		const mecab_node_t *node;
		TupleDesc	tupdesc;

		if (get_call_result_type(fcinfo, NULL, &tupdesc) != TYPEFUNC_COMPOSITE)
			elog(ERROR, "return and sql tuple descriptions are incompatible");

		node = mecab_sparse_tonode2(mecab,
				VARDATA_ANY(txt), VARSIZE_ANY_EXHDR(txt));
		mecab_assert(node);

		funcctx = SRF_FIRSTCALL_INIT();

		tuples = NIL;
		for (; node != NULL; node = node->next)
		{
			int		i;
			Datum		values[NUM_CSV+1];
			bool		nulls[NUM_CSV+1] = { 0 };
			const char	   *csv;
			const char         *conjtype;
			int                conjlen;
			const char *pluspos;
			const char *slashpos;

			

			MemoryContext	ctx;

			/* 시작과 끝 무시 */
			switch (node->stat)
			{
			case MECAB_BOS_NODE:
			case MECAB_EOS_NODE:
				continue;
			}

			/* 단어 처리
                         * conjtype 값이 Inflect 이면, 
                         * detail 기준으로 row로 분리 */

			csv = node->feature;
			if ((feature(node, MECAB_CONJTYPE, &conjtype, &conjlen))
			    && (strncmp(conjtype, "Inflect,", 8) == 0)
			    && (feature(node, MECAB_DETAIL, &conjtype, &conjlen))){
				/* 용언 상세 정보로 처리, 없으면 그대로 */
				do {
					pluspos = strchr(conjtype, '+');
					slashpos = strchr(conjtype , '/');
					values[0] = make_text(conjtype, slashpos - conjtype);
					for (i = 1; i <= NUM_CSV; i++)
					{
						if(i == 1){
							values[i] = make_text(slashpos + 1,
							strchr(slashpos + 1, '/') - slashpos - 1);
						}
						else if(i==3){
							values[i] = make_text("F",1);
						}
						else if(i==4){
							values[i] = make_text(conjtype, slashpos - conjtype);
						}
						else {
							nulls[i] = true;
						}
					}
					ctx = MemoryContextSwitchTo(funcctx->multi_call_memory_ctx);
					tuple = heap_form_tuple(tupdesc, values, nulls);
					tuples = lappend(tuples, tuple);
					MemoryContextSwitchTo(ctx);
					conjtype = pluspos + 1;
				} while(pluspos != NULL);
			}
			else {
				values[0] = make_text(node->surface, node->length);


				for (i = 1; i <= NUM_CSV; i++)
				{
					const char *n = strchr(csv, ',');
					size_t		len = (n == NULL ? strlen(csv) : n - csv);

					if (len == 0 || (len == 1 && csv[0] == '*'))
					{
						if (i == MECAB_BASIC + 1)
							values[i] = make_text(node->surface, node->length);
						else
							nulls[i] = true;
					}
					else{
						values[i] = make_text(csv, len);
					}

					if (n == NULL)
					{
						for (++i; i <= MECAB_BASIC; i++)
							nulls[i] = true;
						/* 未知語 */
						for (; i <= NUM_CSV; i++)
							values[i] = make_text(node->surface, node->length);
						break;
					}

					csv = n + 1;
				}
				ctx = MemoryContextSwitchTo(funcctx->multi_call_memory_ctx);
				tuple = heap_form_tuple(tupdesc, values, nulls);
				tuples = lappend(tuples, tuple);
				MemoryContextSwitchTo(ctx);
			}

		}

		funcctx->max_calls = list_length(tuples);
		funcctx->user_fctx = tuples;

		PG_FREE_IF_COPY(txt, 0);
	}
	else
	{
		funcctx = SRF_PERCALL_SETUP();
		tuples = funcctx->user_fctx;
	}

	if (tuples == NIL)
		SRF_RETURN_DONE(funcctx);

	tuple = linitial(tuples);
	funcctx->user_fctx = tuples = list_delete_first(tuples);
	result = heap_copytuple(tuple);
	pfree(tuple);

	SRF_RETURN_NEXT(funcctx, HeapTupleGetDatum(result));
}

/*
 * korean_normalize - normalize 함수 랩퍼
 * 
 */
Datum
korean_normalize(PG_FUNCTION_ARGS)
{
	Datum			r;
	text		   *txt = PG_GETARG_TEXT_PP(0);
	StringInfoData	str;

	initStringInfo(&str);
	normalize(&str, VARDATA_ANY(txt), VARSIZE_ANY_EXHDR(txt), (append_t) appendBinaryStringInfo);
	PG_FREE_IF_COPY(txt, 0);

	r = CStringGetTextDatum(str.data);
	pfree(str.data);

	PG_RETURN_DATUM(r);
}

/*
 * hanja2hangul - 한자를 한글로 변환
 */
Datum
hanja2hangul(PG_FUNCTION_ARGS)
{
	mecab_t		   *mecab = mecab_acquire();
	text		   *txt = PG_GETARG_TEXT_PP(0);
	StringInfoData	str;
	const mecab_node_t *node;

	node = mecab_sparse_tonode2(mecab,
			VARDATA_ANY(txt), VARSIZE_ANY_EXHDR(txt));
	mecab_assert(node);

	initStringInfo(&str);

	for (; node != NULL; node = node->next)
	{
		const char	*sori;
		int		sorilen;

		switch (node->stat)
		{
		case MECAB_BOS_NODE:
		case MECAB_EOS_NODE:
			continue;
		}

		if (feature(node, MECAB_BASIC, &sori, &sorilen))
			appendBinaryStringInfo(&str, sori, sorilen);
		else
			appendBinaryStringInfo(&str, node->surface, node->length);
		if(node->next != NULL) appendBinaryStringInfo(&str, " ", 1);
	}

	PG_FREE_IF_COPY(txt, 0);

	PG_RETURN_DATUM(CStringGetTextDatum(str.data));
}


/*
 * feature - CSV위치에 * 나, 빈값이 아니면, 그 위치와 길이 반환
 */
static bool
feature(const mecab_node_t *node, int n, const char **t, int *tlen)
{
	const char *csv = node->feature;
	int			i;
	const char *next;
	size_t		len;

	for (i = 0; i < n; i++)
	{
		next = strchr(csv, ',');
		if (next == NULL)
			return false;
		csv = next + 1;
	}

	next = strchr(csv, ',');
	len = (next == NULL ? strlen(csv) : next - csv);

	if (len == 0 || (len == 1 && csv[0] == '*'))
		return false;

	*t = csv;
	*tlen = len;
	return true;
}

/* 3byte 문자 가운데, ascii 코드 문자로 바꿀 수 있는 것들은 바꿈 */
static bool ismbascii(const unsigned char *s, unsigned char *c, int *cnt){
	pg_wchar ch;

	ch = utf8_to_unicode(s);
	/* 공백 */
	if(ch == 0x3000){
		*cnt = 1;
		ch = 0x20;
		unicode_to_utf8(ch, c);
	}
	/* 전각 아스키 */
	else if(ch >= 0xFF01 && ch <= 0xff5e){
		*cnt = 1;
		ch = ch - (0xff01) + 0x21;
		unicode_to_utf8(ch, c);
	}
	else if(ch == 0x24ea || ch == 0x24ff){
		*cnt = 1;
		sprintf((char *)c, "0");
	}
	/* 원문자 숫자들 1~20 */
	else if(ch >= 0x2460 && ch <= 0x249b){
		sprintf((char *)c, "%d", (ch - 0x2460 ) % 20 + 1);
		*cnt = strlen((const char *)c);
	}
	/* 알파벳 번호 */
	else if(ch >= 0x249c && ch <= 0x24e9) {
		sprintf((char *)c, "%c ", (char)((ch - 0x249c) % 26 + 0x61));
		*cnt = strlen((const char *)c);
	}
	/* 11 ~ 20 */
	else if(ch >= 0x24eb && ch <= 0x24f4) {
		sprintf((char *)c, "%d", ch - 0x24eb + 11 );
		*cnt = strlen((const char *)c);
	}
	/* 1 ~ 10 */
	else if(ch >= 0x24f5 && ch <= 0x24fe) {
		sprintf((char *)c, "%d", ch - 0x24eb + 1 );
		*cnt = strlen((const char *)c);
	}
	/* Enclosed CJK Letters and Months 부분은 생략함 */
	else{
		return false;
	}
	return true;
}

/*
 * normalize - 문자정리
 * 영숫자 : 전각 -> 반각
 * 영어, 숫자가 부분 포함 된 것을 공백으로 분리
 * TODO 
 */
static void
normalize(StringInfo dst, const char *src, size_t srclen, append_t append)
{
	int len, nextcharlen, current_len;
	const unsigned char *s = (const unsigned char *)src;
        const unsigned char *end = s + srclen;
	unsigned char *newch;
	newch = (unsigned char *) palloc(4);
	for (; s < end; s += len){
		//ch = s[0];
		len = uchar_mblen(s);
		if(len == 3 && ismbascii(s, newch, &current_len)){
			appendBinaryStringInfo(dst, (const char *)newch, current_len);
		}
		else {
			appendBinaryStringInfo(dst, (const char *)s, len);
			current_len = len;
		}

		/* 3byte 이상 문자와 미만 문자가 공백 없이 이어지면 공백문자 넣음 */
		/* 처리 안하면 mecab 쪽에서 분석 못함 */
		if((s + len) < end){
			nextcharlen = uchar_mblen(s + len);
			if(nextcharlen > 2) {
				ismbascii(s + len, newch, &nextcharlen);
			}
			if((current_len < 3 && nextcharlen > 2 && (s)[0] != 0x20)
				|| (current_len > 2 && nextcharlen < 3 && (s+len)[0] != 0x20))
				appendBinaryStringInfo(dst, " ", 1);
		}
	}
	pfree(newch);
}

/*
 * lexize - mecab 처리 결과 버퍼에서 단어 추출
 */
static char *
lexize(const char *str, size_t len)
{
	char *r;

		r = palloc(len + 1);
		memcpy(r, str, len);
		r[len] = '\0';

	return r;
}

static bool
accept_mecab_ko_part(const char* str, int slen){
	bool isfind = false;
	int i=0;
	char input_str[15];
	strncpy(input_str, str, slen);
	input_str[slen] = '\0';
	while(1){
		if(strcmp(accept_parts_of_speech[i], "") == 0) break;
		if(strncmp(accept_parts_of_speech[i], input_str, slen +1) == 0){
			isfind = true;
			break;
		}
		i += 1;
	}
	return isfind;
}

/*
 * 2-C: has_hangul
 * 문자열에 한글 음절(Hangul Syllables U+AC00..U+D7A3) 포함 여부 확인.
 * UTF-8 범위: EA B0 80 .. ED 9E A3
 */
static bool
has_hangul(const char *s, int len)
{
	const unsigned char *p = (const unsigned char *) s;
	const unsigned char *end = p + len;

	while (p < end)
	{
		int clen = pg_mblen((const char *) p);
		if (clen == 3 &&
			((p[0] == 0xEA && p[1] >= 0xB0) ||
			 p[0] == 0xEB || p[0] == 0xEC ||
			 (p[0] == 0xED && p[1] <= 0x9E)))
			return true;
		p += clen;
	}
	return false;
}

/*
 * 2-C: ends_with_hada
 * 기본형이 "하다" (U+D558 U+B2E4 = ED 95 98 EB 8B A4, 6 bytes) 로 끝나는지 확인.
 */
static const unsigned char hada_utf8[HADA_BYTES] = {
	0xED, 0x95, 0x98,   /* 하 */
	0xEB, 0x8B, 0xA4    /* 다 */
};

static bool
ends_with_hada(const char *s, int slen)
{
	return slen > HADA_BYTES &&
		memcmp(s + slen - HADA_BYTES, hada_utf8, HADA_BYTES) == 0;
}

/*
 * 2-C: compound_lexemes
 * mecab-ko-dic field4 복합명사 분해 정보("정보/NNG+검색/NNG")를 파싱하여
 * 복합체 기본형 + 각 구성 명사를 TSLexeme 배열로 반환.
 *
 * 반환 형식: { "정보검색", "정보", "검색", NULL-terminated }
 * PostgreSQL tsvector는 동일 position 다중 lexeme을 허용하므로
 * 복합어 phrase matching과 partial-term recall을 동시에 제공.
 */
static TSLexeme *
compound_lexemes(const char *base, int baselen,
				 const char *compound, int complen)
{
	int			n = 1;
	int			idx = 0;
	const char *p;
	const char *end;
	const char *slash;
	const char *plus;
	TSLexeme   *res;

	/* '+' 개수 = 구성요소 수 - 1 */
	for (int i = 0; i < complen; i++)
		if (compound[i] == '+') n++;

	/* 복합체(1) + 구성요소(n) + 종결자(1) */
	res = palloc0(sizeof(TSLexeme) * (n + 2));

	/* 복합체 기본형 */
	res[idx++].lexeme = lexize(base, baselen);

	/* 각 구성요소: "word/TAG+word/TAG+..." 파싱 */
	p = compound;
	end = compound + complen;
	while (p < end)
	{
		slash = memchr(p, '/', end - p);
		if (!slash || slash == p)
			break;
		res[idx++].lexeme = lexize(p, slash - p);
		plus = memchr(slash + 1, '+', end - slash - 1);
		p = plus ? plus + 1 : end;
	}

	return res;  /* 마지막 항목은 palloc0으로 NULL 초기화됨 (TSLexeme terminator) */
}

/*
 * 줄바꿈 문자가 있을 경우, 영어와 한국어 처리를 다르게 함
 */
static void
appendString(StringInfo dst, const unsigned char *src, int srclen)
{
        /* 1byte 출력할 수 없는 것은 \v로 */
	if (srclen == 1 && !isprint(src[0]))
	{
		if (dst->len == 0 || *StringInfoTail(dst, 1) != SEPARATOR_CHAR)
			appendStringInfoChar(dst, SEPARATOR_CHAR);
	}
	else if (dst->len > 1)
	{
		if (*StringInfoTail(dst, 1) == SEPARATOR_CHAR)
		{
			bool ishigh = IS_HIGHBIT_SET(*StringInfoTail(dst, 2));
			if (srclen == 1 && !ishigh)
			{
				/* "A[줄바꿈]A" ⇒ "A A" */
				*StringInfoTail(dst, 1) = ' ';
			}
			else if (ishigh)
			{
				/* "아[줄바끔]아" ⇒ "아아" */
				dst->len--;
			}
		}
		else
		{
			bool ishigh = IS_HIGHBIT_SET(*StringInfoTail(dst, 1));
			if ((srclen == 1 && ishigh) || (srclen > 1 && !ishigh))
				appendStringInfoChar(dst, SEPARATOR_CHAR);
		}
		appendBinaryStringInfo(dst, (const char *) src, srclen);
	}
	else
	{
		appendBinaryStringInfo(dst, (const char *) src, srclen);
	}
}
