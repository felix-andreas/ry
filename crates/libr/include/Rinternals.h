/*
 * ry's declaration of R's C API, source-compatible with R 4.6 for the names ry implements.
 * SEXP is opaque, as it is in GNU R since 4.2.0: every accessor is a function exported by libR.
 */
#ifndef R_INTERNALS_H_
#define R_INTERNALS_H_

#include <Rversion.h>

#ifdef __cplusplus
#include <cstdio>
#include <climits>
#include <cstddef>
#include <cstdint>
extern "C" {
#else
#include <stdio.h>
#include <limits.h>
#include <stddef.h>
#include <stdint.h>
#include <errno.h>
#endif

#include <R_ext/Arith.h>
#include <R_ext/Boolean.h>
#include <R_ext/Complex.h>
#include <R_ext/Error.h>
#include <R_ext/Memory.h>
#include <R_ext/Utils.h>
#include <R_ext/Print.h>
#include <R_ext/Rdynload.h>
#include <R_ext/Rallocators.h>
#include <R_ext/libextern.h>

#define R_INLINE inline

typedef unsigned char Rbyte;
typedef int R_len_t;
typedef ptrdiff_t R_xlen_t;

#define R_LEN_T_MAX INT_MAX
#define R_XLEN_T_MAX 4503599627370496
#define R_SHORT_LEN_MAX 2147483647
#define LONG_VECTOR_SUPPORT
#define R_PRIdXLEN_T "td"

typedef unsigned int SEXPTYPE;

#define NILSXP 0
#define SYMSXP 1
#define LISTSXP 2
#define CLOSXP 3
#define ENVSXP 4
#define PROMSXP 5
#define LANGSXP 6
#define SPECIALSXP 7
#define BUILTINSXP 8
#define CHARSXP 9
#define LGLSXP 10
#define INTSXP 13
#define REALSXP 14
#define CPLXSXP 15
#define STRSXP 16
#define DOTSXP 17
#define ANYSXP 18
#define VECSXP 19
#define EXPRSXP 20
#define BCODESXP 21
#define EXTPTRSXP 22
#define WEAKREFSXP 23
#define RAWSXP 24
#define OBJSXP 25
#define S4SXP 25
#define NEWSXP 30
#define FREESXP 31
#define FUNSXP 99

typedef struct SEXPREC *SEXP;

typedef enum {
    CE_NATIVE = 0,
    CE_UTF8 = 1,
    CE_LATIN1 = 2,
    CE_BYTES = 3,
    CE_SYMBOL = 5,
    CE_ANY = 99
} cetype_t;

typedef int PROTECT_INDEX;
typedef void (*R_CFinalizer_t)(SEXP);

/* The header. */
int (TYPEOF)(SEXP x);
int (OBJECT)(SEXP x);
void (SET_OBJECT)(SEXP x, int v);
int (IS_S4_OBJECT)(SEXP x);
void (SET_S4_OBJECT)(SEXP x);
void (UNSET_S4_OBJECT)(SEXP x);
int (LEVELS)(SEXP x);
int (SETLEVELS)(SEXP x, int v);
int (NAMED)(SEXP x);
int (REFCNT)(SEXP x);
void (SET_NAMED)(SEXP x, int v);
void (MARK_NOT_MUTABLE)(SEXP x);
int (ALTREP)(SEXP x);
SEXP (ATTRIB)(SEXP x);
void SET_ATTRIB(SEXP x, SEXP v);
/* The headers declare the API of the R version ry presents (Rversion.h), so a package's version
 * branches see the same names they would under that GNU R: rlang and yaml define their own
 * R_mkClosure and ANY_ATTRIB below 4.5. */
#if R_VERSION >= R_Version(4, 5, 0)
int (ANY_ATTRIB)(SEXP x);
void (CLEAR_ATTRIB)(SEXP x);
#endif
void DUPLICATE_ATTRIB(SEXP to, SEXP from);
void SHALLOW_DUPLICATE_ATTRIB(SEXP to, SEXP from);

#define MAYBE_SHARED(x) (NAMED(x) > 1)
#define NO_REFERENCES(x) (NAMED(x) == 0)
#define MAYBE_REFERENCED(x) (!NO_REFERENCES(x))
#define NOT_SHARED(x) (!MAYBE_SHARED(x))
int (MAYBE_SHARED)(SEXP x);
int (NO_REFERENCES)(SEXP x);
int (IS_SCALAR)(SEXP x, int type);

/* Vectors. */
int (LENGTH)(SEXP x);
R_xlen_t (XLENGTH)(SEXP x);
#define IS_LONG_VEC(x) (XLENGTH(x) > R_SHORT_LEN_MAX)
int (IS_LONG_VEC)(SEXP x);
/* The checked forms of LENGTH and XLENGTH and the data pointers without the ALTREP check, which
   GNU R's inline accessors expand to. */
int LENGTH_EX(SEXP x, const char *file, int line);
R_xlen_t XLENGTH_EX(SEXP x);
int *LOGICAL0(SEXP x);
int *INTEGER0(SEXP x);
double *REAL0(SEXP x);
Rcomplex *COMPLEX0(SEXP x);
Rbyte *RAW0(SEXP x);
int *(LOGICAL)(SEXP x);
int *(INTEGER)(SEXP x);
Rbyte *(RAW)(SEXP x);
double *(REAL)(SEXP x);
Rcomplex *(COMPLEX)(SEXP x);
const int *(LOGICAL_RO)(SEXP x);
const int *(INTEGER_RO)(SEXP x);
const Rbyte *(RAW_RO)(SEXP x);
const double *(REAL_RO)(SEXP x);
const Rcomplex *(COMPLEX_RO)(SEXP x);
void *(DATAPTR)(SEXP x);
const void *(DATAPTR_RO)(SEXP x);
const void *(DATAPTR_OR_NULL)(SEXP x);
void *(STDVEC_DATAPTR)(SEXP x);
const int *(LOGICAL_OR_NULL)(SEXP x);
const int *(INTEGER_OR_NULL)(SEXP x);
const double *(REAL_OR_NULL)(SEXP x);
const Rcomplex *(COMPLEX_OR_NULL)(SEXP x);
const Rbyte *(RAW_OR_NULL)(SEXP x);
SEXP (STRING_ELT)(SEXP x, R_xlen_t i);
SEXP (VECTOR_ELT)(SEXP x, R_xlen_t i);
void SET_STRING_ELT(SEXP x, R_xlen_t i, SEXP v);
SEXP SET_VECTOR_ELT(SEXP x, R_xlen_t i, SEXP v);
SEXP *(STRING_PTR)(SEXP x);
const SEXP *(STRING_PTR_RO)(SEXP x);
#if R_VERSION >= R_Version(4, 5, 0)
const SEXP *(VECTOR_PTR_RO)(SEXP x);
#endif
/* GNU R declares VECTOR_PTR noreturn from 4.5 on because it refuses the call; ry hands out the
 * pointer, so the declaration stays an ordinary one. */
SEXP *(VECTOR_PTR)(SEXP x);
int INTEGER_ELT(SEXP x, R_xlen_t i);
double REAL_ELT(SEXP x, R_xlen_t i);
int LOGICAL_ELT(SEXP x, R_xlen_t i);
Rcomplex COMPLEX_ELT(SEXP x, R_xlen_t i);
Rbyte RAW_ELT(SEXP x, R_xlen_t i);
void SET_INTEGER_ELT(SEXP x, R_xlen_t i, int v);
void SET_REAL_ELT(SEXP x, R_xlen_t i, double v);
void SET_LOGICAL_ELT(SEXP x, R_xlen_t i, int v);
void SET_COMPLEX_ELT(SEXP x, R_xlen_t i, Rcomplex v);
void SET_RAW_ELT(SEXP x, R_xlen_t i, Rbyte v);
R_xlen_t INTEGER_GET_REGION(SEXP sx, R_xlen_t i, R_xlen_t n, int *buf);
R_xlen_t REAL_GET_REGION(SEXP sx, R_xlen_t i, R_xlen_t n, double *buf);
R_xlen_t LOGICAL_GET_REGION(SEXP sx, R_xlen_t i, R_xlen_t n, int *buf);
R_xlen_t COMPLEX_GET_REGION(SEXP sx, R_xlen_t i, R_xlen_t n, Rcomplex *buf);
R_xlen_t RAW_GET_REGION(SEXP sx, R_xlen_t i, R_xlen_t n, Rbyte *buf);
int INTEGER_IS_SORTED(SEXP x);
int INTEGER_NO_NA(SEXP x);
int REAL_IS_SORTED(SEXP x);
int REAL_NO_NA(SEXP x);
int LOGICAL_IS_SORTED(SEXP x);
int LOGICAL_NO_NA(SEXP x);
int STRING_IS_SORTED(SEXP x);
int STRING_NO_NA(SEXP x);

#define UNKNOWN_SORTEDNESS INT_MIN
#define SORTED_DECR_NA_1ST -2
#define SORTED_DECR -1
#define SORTED_INCR 1
#define SORTED_INCR_NA_1ST 2
#define KNOWN_UNSORTED 0
#define KNOWN_SORTED(sorted) (sorted == SORTED_DECR || sorted == SORTED_INCR || sorted == SORTED_DECR_NA_1ST || sorted == SORTED_INCR_NA_1ST)
#define KNOWN_NA_1ST(sorted) (sorted == SORTED_INCR_NA_1ST || sorted == SORTED_DECR_NA_1ST)
#define KNOWN_INCR(sorted) (sorted == SORTED_INCR || sorted == SORTED_INCR_NA_1ST)
#define KNOWN_DECR(sorted) (sorted == SORTED_DECR || sorted == SORTED_DECR_NA_1ST)

/* ALTREP instances: their data slots and class, the class's name and package symbols, and a
   wrapper that can carry sortedness metadata. The class interface is in R_ext/Altrep.h. */
SEXP ALTREP_CLASS(SEXP x);
SEXP R_altrep_data1(SEXP x);
SEXP R_altrep_data2(SEXP x);
void R_set_altrep_data1(SEXP x, SEXP v);
void R_set_altrep_data2(SEXP x, SEXP v);
#if R_VERSION >= R_Version(4, 6, 0)
SEXP R_altrep_class_name(SEXP x);
SEXP R_altrep_class_package(SEXP x);
#endif
SEXP R_tryWrap(SEXP x);

#if R_VERSION >= R_Version(4, 6, 0)
SEXP R_allocResizableVector(SEXPTYPE type, R_xlen_t maxlen);
void R_resizeVector(SEXP x, R_xlen_t newlen);
Rboolean R_isResizable(SEXP x);
R_xlen_t R_maxLength(SEXP x);
#endif

/* Strings. */
const char *(R_CHAR)(SEXP x);
#define CHAR(x) R_CHAR(x)
int (IS_ASCII)(SEXP x);
int (IS_UTF8)(SEXP x);
int (IS_LATIN1)(SEXP x);
int (IS_BYTES)(SEXP x);
#if R_VERSION >= R_Version(4, 5, 0)
Rboolean Rf_charIsASCII(SEXP x);
Rboolean Rf_charIsUTF8(SEXP x);
Rboolean Rf_charIsLatin1(SEXP x);
#endif
SEXP Rf_mkChar(const char *);
SEXP Rf_mkCharCE(const char *, cetype_t);
SEXP Rf_mkCharLen(const char *, int);
SEXP Rf_mkCharLenCE(const char *, int, cetype_t);
SEXP Rf_mkString(const char *);
cetype_t Rf_getCharCE(SEXP);
const char *Rf_translateChar(SEXP);
const char *Rf_translateCharUTF8(SEXP);
const char *Rf_translateCharFP(SEXP);
const char *Rf_reEnc(const char *x, cetype_t ce_in, cetype_t ce_out, int subst);
SEXP Rf_install(const char *);
SEXP Rf_installChar(SEXP);
SEXP Rf_installTrChar(SEXP);
Rboolean Rf_StringBlank(SEXP);
Rboolean Rf_isValidString(SEXP);
Rboolean Rf_isValidStringF(SEXP);
#define NA_STRING R_NaString

/* Pairlists and language objects. */
SEXP (TAG)(SEXP e);
SEXP (CAR)(SEXP e);
SEXP (CDR)(SEXP e);
SEXP (CAAR)(SEXP e);
SEXP (CDAR)(SEXP e);
SEXP (CADR)(SEXP e);
SEXP (CDDR)(SEXP e);
SEXP (CDDDR)(SEXP e);
SEXP (CADDR)(SEXP e);
SEXP (CADDDR)(SEXP e);
SEXP (CAD4R)(SEXP e);
int (MISSING)(SEXP x);
void SET_TAG(SEXP x, SEXP y);
SEXP SETCAR(SEXP x, SEXP y);
SEXP SETCDR(SEXP x, SEXP y);
SEXP SETCADR(SEXP x, SEXP y);
SEXP SETCADDR(SEXP x, SEXP y);
SEXP SETCADDDR(SEXP x, SEXP y);
SEXP SETCAD4R(SEXP e, SEXP y);
#define CONS(a, b) Rf_cons((a), (b))
#define LCONS(a, b) Rf_lcons((a), (b))
SEXP Rf_cons(SEXP, SEXP);
SEXP Rf_lcons(SEXP, SEXP);
SEXP Rf_list1(SEXP);
SEXP Rf_list2(SEXP, SEXP);
SEXP Rf_list3(SEXP, SEXP, SEXP);
SEXP Rf_list4(SEXP, SEXP, SEXP, SEXP);
SEXP Rf_list5(SEXP, SEXP, SEXP, SEXP, SEXP);
SEXP Rf_list6(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP);
SEXP Rf_lang1(SEXP);
SEXP Rf_lang2(SEXP, SEXP);
SEXP Rf_lang3(SEXP, SEXP, SEXP);
SEXP Rf_lang4(SEXP, SEXP, SEXP, SEXP);
SEXP Rf_lang5(SEXP, SEXP, SEXP, SEXP, SEXP);
SEXP Rf_lang6(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP);
SEXP Rf_elt(SEXP, int);
SEXP Rf_lastElt(SEXP);
SEXP Rf_nthcdr(SEXP, int);
SEXP Rf_listAppend(SEXP, SEXP);
SEXP Rf_PairToVectorList(SEXP x);
SEXP Rf_VectorToPairList(SEXP x);
SEXP (PRINTNAME)(SEXP x);

/* Closures, promises and environments. */
SEXP (FORMALS)(SEXP x);
SEXP (BODY)(SEXP x);
SEXP (CLOENV)(SEXP x);
#if R_VERSION >= R_Version(4, 5, 0)
SEXP R_ClosureFormals(SEXP x);
SEXP R_ClosureBody(SEXP x);
SEXP R_ClosureEnv(SEXP x);
SEXP R_mkClosure(SEXP formals, SEXP body, SEXP env);
#endif
SEXP (PRCODE)(SEXP x);
SEXP (PRENV)(SEXP x);
SEXP (PRVALUE)(SEXP x);
void SET_PRCODE(SEXP x, SEXP v);
void SET_PRENV(SEXP x, SEXP v);
void SET_PRVALUE(SEXP x, SEXP v);
SEXP R_PromiseExpr(SEXP x);
SEXP (PREXPR)(SEXP x);
SEXP (ENCLOS)(SEXP x);
#if R_VERSION >= R_Version(4, 5, 0)
SEXP R_ParentEnv(SEXP x);
#endif
SEXP R_NewEnv(SEXP enclos, int hash, int size);
void Rf_defineVar(SEXP symbol, SEXP value, SEXP rho);
void Rf_setVar(SEXP symbol, SEXP value, SEXP rho);
SEXP Rf_findVar(SEXP symbol, SEXP rho);
SEXP Rf_findVarInFrame(SEXP rho, SEXP symbol);
SEXP Rf_findVarInFrame3(SEXP rho, SEXP symbol, Rboolean doGet);
#if R_VERSION >= R_Version(4, 5, 0)
SEXP R_getVar(SEXP sym, SEXP rho, Rboolean inherits);
SEXP R_getVarEx(SEXP sym, SEXP rho, Rboolean inherits, SEXP ifnotfound);
#endif
Rboolean R_existsVarInFrame(SEXP rho, SEXP symbol);
void R_removeVarFromFrame(SEXP symbol, SEXP env);
SEXP Rf_findFun(SEXP symbol, SEXP rho);
void R_LockEnvironment(SEXP env, Rboolean bindings);
Rboolean R_EnvironmentIsLocked(SEXP env);
void R_LockBinding(SEXP sym, SEXP env);
void R_unLockBinding(SEXP sym, SEXP env);
Rboolean R_BindingIsLocked(SEXP sym, SEXP env);
Rboolean R_BindingIsActive(SEXP sym, SEXP env);
SEXP R_lsInternal(SEXP env, Rboolean all);
SEXP R_lsInternal3(SEXP env, Rboolean all, Rboolean sorted);
SEXP R_FindNamespace(SEXP info);
Rboolean R_IsNamespaceEnv(SEXP rho);
Rboolean R_IsPackageEnv(SEXP rho);
SEXP Rf_GetOption1(SEXP tag);

/* Allocation and protection. */
SEXP Rf_allocVector(SEXPTYPE, R_xlen_t);
SEXP Rf_allocVector3(SEXPTYPE, R_xlen_t, R_allocator_t *);
SEXP Rf_allocMatrix(SEXPTYPE, int, int);
SEXP Rf_alloc3DArray(SEXPTYPE, int, int, int);
SEXP Rf_allocArray(SEXPTYPE, SEXP);
SEXP Rf_allocList(int);
#if R_VERSION >= R_Version(4, 5, 0)
SEXP Rf_allocLang(int);
#endif
SEXP Rf_allocS4Object(void);
SEXP Rf_allocSExp(SEXPTYPE);
SEXP Rf_ScalarLogical(int);
SEXP Rf_ScalarInteger(int);
SEXP Rf_ScalarReal(double);
SEXP Rf_ScalarComplex(Rcomplex);
SEXP Rf_ScalarString(SEXP);
SEXP Rf_ScalarRaw(Rbyte);
SEXP Rf_mkNamed(SEXPTYPE, const char **);
SEXP Rf_duplicate(SEXP);
SEXP Rf_shallow_duplicate(SEXP);
SEXP Rf_lazy_duplicate(SEXP);
SEXP Rf_lengthgets(SEXP, R_len_t);
SEXP Rf_xlengthgets(SEXP, R_xlen_t);
SEXP Rf_protect(SEXP);
void R_signal_protect_error(void);
void R_BadLongVector(SEXP x, const char *file, int line);
void Rf_unprotect(int);
void Rf_unprotect_ptr(SEXP);
void R_ProtectWithIndex(SEXP, PROTECT_INDEX *);
void R_Reprotect(SEXP, PROTECT_INDEX);
void R_PreserveObject(SEXP);
void R_ReleaseObject(SEXP);
SEXP R_NewPreciousMSet(int);
void R_PreserveInMSet(SEXP x, SEXP mset);
void R_ReleaseFromMSet(SEXP x, SEXP mset);
void R_ReleaseMSet(SEXP mset, int keepSize);

#define PROTECT(s) Rf_protect(s)
#define UNPROTECT(n) Rf_unprotect(n)
#define UNPROTECT_PTR(s) Rf_unprotect_ptr(s)
#define PROTECT_WITH_INDEX(x, i) R_ProtectWithIndex(x, i)
#define REPROTECT(x, i) R_Reprotect(x, i)

/* Attributes and classes. */
SEXP Rf_getAttrib(SEXP, SEXP);
SEXP Rf_setAttrib(SEXP, SEXP, SEXP);
void Rf_copyMostAttrib(SEXP, SEXP);
SEXP Rf_classgets(SEXP, SEXP);
SEXP Rf_namesgets(SEXP, SEXP);
SEXP Rf_dimgets(SEXP, SEXP);
SEXP Rf_dimnamesgets(SEXP, SEXP);
SEXP Rf_GetRowNames(SEXP);
Rboolean Rf_inherits(SEXP, const char *);
#if R_VERSION >= R_Version(4, 6, 0)
SEXP R_mapAttrib(SEXP x, SEXP (*FUN)(SEXP, SEXP, void *), void *data);
#endif

/* Type predicates and coercion. */
Rboolean Rf_isNull(SEXP s);
Rboolean Rf_isSymbol(SEXP s);
Rboolean Rf_isLogical(SEXP s);
Rboolean Rf_isReal(SEXP s);
Rboolean Rf_isComplex(SEXP s);
Rboolean Rf_isExpression(SEXP s);
Rboolean Rf_isEnvironment(SEXP s);
Rboolean Rf_isString(SEXP s);
Rboolean Rf_isObject(SEXP s);
Rboolean Rf_isInteger(SEXP s);
Rboolean Rf_isFactor(SEXP s);
Rboolean Rf_isOrdered(SEXP s);
Rboolean Rf_isUnordered(SEXP s);
Rboolean Rf_isNumeric(SEXP s);
Rboolean Rf_isNumber(SEXP s);
Rboolean Rf_isVector(SEXP s);
Rboolean Rf_isVectorAtomic(SEXP s);
Rboolean Rf_isVectorList(SEXP s);
Rboolean Rf_isList(SEXP s);
Rboolean Rf_isNewList(SEXP s);
Rboolean Rf_isPairList(SEXP s);
Rboolean Rf_isLanguage(SEXP s);
Rboolean Rf_isMatrix(SEXP s);
Rboolean Rf_isArray(SEXP s);
Rboolean Rf_isTs(SEXP s);
Rboolean Rf_isFunction(SEXP s);
Rboolean Rf_isPrimitive(SEXP s);
Rboolean Rf_isFrame(SEXP s);
#if R_VERSION >= R_Version(4, 5, 0)
Rboolean Rf_isDataFrame(SEXP s);
#endif
Rboolean Rf_isS4(SEXP s);
R_len_t Rf_length(SEXP);
R_xlen_t Rf_xlength(SEXP);
int Rf_nrows(SEXP);
int Rf_ncols(SEXP);
SEXP Rf_coerceVector(SEXP, SEXPTYPE);
SEXP Rf_asCharacterFactor(SEXP x);
int Rf_asLogical(SEXP x);
int Rf_asLogical2(SEXP x, int checking, SEXP call);
int Rf_asInteger(SEXP x);
double Rf_asReal(SEXP x);
SEXP Rf_asChar(SEXP x);
R_xlen_t Rf_asXLength(SEXP x);
const char *Rf_type2char(SEXPTYPE);
SEXP Rf_type2str(SEXPTYPE);
SEXP Rf_type2str_nowarn(SEXPTYPE);
SEXPTYPE Rf_str2type(const char *);
Rboolean R_compute_identical(SEXP, SEXP, int);
SEXP Rf_match(SEXP table, SEXP x, int nomatch);

/* External pointers, weak references and finalizers. */
SEXP R_MakeExternalPtr(void *p, SEXP tag, SEXP prot);
void *R_ExternalPtrAddr(SEXP s);
SEXP R_ExternalPtrTag(SEXP s);
SEXP R_ExternalPtrProtected(SEXP s);
void R_ClearExternalPtr(SEXP s);
void R_SetExternalPtrAddr(SEXP s, void *p);
void R_SetExternalPtrTag(SEXP s, SEXP tag);
void R_SetExternalPtrProtected(SEXP s, SEXP p);
SEXP R_MakeExternalPtrFn(DL_FUNC p, SEXP tag, SEXP prot);
DL_FUNC R_ExternalPtrAddrFn(SEXP s);
void R_RegisterFinalizer(SEXP s, SEXP fun);
void R_RegisterCFinalizer(SEXP s, R_CFinalizer_t fun);
void R_RegisterFinalizerEx(SEXP s, SEXP fun, Rboolean onexit);
void R_RegisterCFinalizerEx(SEXP s, R_CFinalizer_t fun, Rboolean onexit);
void R_RunPendingFinalizers(void);
SEXP R_MakeWeakRef(SEXP key, SEXP val, SEXP fin, Rboolean onexit);
SEXP R_MakeWeakRefC(SEXP key, SEXP val, R_CFinalizer_t fin, Rboolean onexit);
SEXP R_WeakRefKey(SEXP w);
SEXP R_WeakRefValue(SEXP w);
void R_RunWeakRefFinalizer(SEXP w);

/* Evaluation and non-local exits. */
SEXP Rf_eval(SEXP, SEXP);
SEXP R_tryEval(SEXP, SEXP, int *);
SEXP R_tryEvalSilent(SEXP, SEXP, int *);
const char *R_curErrorBuf(void);
Rboolean R_ToplevelExec(void (*fun)(void *), void *data);
SEXP R_ExecWithCleanup(SEXP (*fun)(void *), void *data, void (*cleanfun)(void *), void *cleandata);
SEXP R_tryCatchError(SEXP (*)(void *), void *, SEXP (*)(SEXP, void *), void *);
SEXP R_tryCatch(SEXP (*)(void *), void *, SEXP, SEXP (*)(SEXP, void *), void *, void (*)(void *), void *);
SEXP R_MakeUnwindCont(void);
NORET void R_ContinueUnwind(SEXP cont);
SEXP R_UnwindProtect(SEXP (*fun)(void *data), void *data,
                     void (*cleanfun)(void *data, Rboolean jump), void *cleandata, SEXP cont);
NORET void Rf_errorcall(SEXP, const char *, ...) R_PRINTF_FORMAT(2, 3);
void Rf_warningcall(SEXP, const char *, ...) R_PRINTF_FORMAT(2, 3);
void Rf_warningcall_immediate(SEXP, const char *, ...) R_PRINTF_FORMAT(2, 3);
void Rf_onintr(void);
LibExtern int R_interrupts_pending;
LibExtern Rboolean R_interrupts_suspended;
LibExtern Rboolean R_Interactive;

/* The constant objects. */
LibExtern SEXP R_NilValue;
LibExtern SEXP R_GlobalEnv;
LibExtern SEXP R_EmptyEnv;
LibExtern SEXP R_BaseEnv;
LibExtern SEXP R_BaseNamespace;
LibExtern SEXP R_NamespaceRegistry;
LibExtern SEXP R_UnboundValue;
LibExtern SEXP R_MissingArg;
LibExtern SEXP R_NaString;
LibExtern SEXP R_BlankString;
LibExtern SEXP R_BlankScalarString;
LibExtern SEXP R_TrueValue;
LibExtern SEXP R_FalseValue;
LibExtern SEXP R_LogicalNAValue;
LibExtern SEXP R_Srcref;
LibExtern Rboolean R_InBCInterpreter;
LibExtern SEXP R_CurrentExpression;
LibExtern SEXP R_MethodsNamespace;

/* Symbols R provides ready-made. */
LibExtern SEXP R_AsCharacterSymbol;
LibExtern SEXP R_BaseSymbol;
LibExtern SEXP R_baseSymbol;
LibExtern SEXP R_BraceSymbol;
LibExtern SEXP R_Bracket2Symbol;
LibExtern SEXP R_BracketSymbol;
LibExtern SEXP R_ClassSymbol;
LibExtern SEXP R_ColonSymbol;
LibExtern SEXP R_DeviceSymbol;
LibExtern SEXP R_DimNamesSymbol;
LibExtern SEXP R_DimSymbol;
LibExtern SEXP R_DollarSymbol;
LibExtern SEXP R_DotEnvSymbol;
LibExtern SEXP R_DotsSymbol;
LibExtern SEXP R_DoubleColonSymbol;
LibExtern SEXP R_DropSymbol;
LibExtern SEXP R_EvalSymbol;
LibExtern SEXP R_ExactSymbol;
LibExtern SEXP R_FunctionSymbol;
LibExtern SEXP R_LastvalueSymbol;
LibExtern SEXP R_LevelsSymbol;
LibExtern SEXP R_ModeSymbol;
LibExtern SEXP R_NaRmSymbol;
LibExtern SEXP R_NameSymbol;
LibExtern SEXP R_NamesSymbol;
LibExtern SEXP R_NamespaceEnvSymbol;
LibExtern SEXP R_PackageSymbol;
LibExtern SEXP R_PreviousSymbol;
LibExtern SEXP R_QuoteSymbol;
LibExtern SEXP R_RowNamesSymbol;
LibExtern SEXP R_SeedsSymbol;
LibExtern SEXP R_SortListSymbol;
LibExtern SEXP R_SourceSymbol;
LibExtern SEXP R_SpecSymbol;
LibExtern SEXP R_SrcfileSymbol;
LibExtern SEXP R_SrcrefSymbol;
LibExtern SEXP R_TripleColonSymbol;
LibExtern SEXP R_TspSymbol;
LibExtern SEXP R_UseNamesSymbol;
LibExtern SEXP R_WholeSrcrefSymbol;
LibExtern SEXP R_dot_defined;
LibExtern SEXP R_dot_Method;
LibExtern SEXP R_dot_packageName;
LibExtern SEXP R_dot_target;
LibExtern SEXP R_dot_Generic;

/* Bindings, dots and namespaces. */
typedef enum {
    R_BindingTypeUnbound = 0,
    R_BindingTypeValue = 1,
    R_BindingTypeMissing = 2,
    R_BindingTypeDelayed = 3,
    R_BindingTypeForced = 4,
    R_BindingTypeActive = 5
} R_BindingType_t;
#if R_VERSION >= R_Version(4, 6, 0)
R_BindingType_t R_GetBindingType(SEXP sym, SEXP env);
#endif
void R_MakeActiveBinding(SEXP sym, SEXP fun, SEXP env);
#if R_VERSION >= R_Version(4, 6, 0)
void R_MakeDelayedBinding(SEXP sym, SEXP expr, SEXP eval_env, SEXP env);
void R_MakeForcedBinding(SEXP sym, SEXP expr, SEXP value, SEXP env);
void R_MakeMissingBinding(SEXP sym, SEXP env);
#endif
SEXP R_ActiveBindingFunction(SEXP sym, SEXP env);
#if R_VERSION >= R_Version(4, 6, 0)
SEXP R_DelayedBindingExpression(SEXP sym, SEXP env);
SEXP R_DelayedBindingEnvironment(SEXP sym, SEXP env);
SEXP R_ForcedBindingExpression(SEXP sym, SEXP env);
#endif
typedef enum {
    R_DotTypeValue = 0,
    R_DotTypeMissing = 1,
    R_DotTypeDelayed = 2,
    R_DotTypeForced = 3
} R_DotType_t;
#if R_VERSION >= R_Version(4, 6, 0)
SEXP R_findDotsEnv(SEXP env);
Rboolean R_DotsExist(SEXP env);
int R_DotsLength(SEXP env);
SEXP R_DotsNames(SEXP env);
SEXP R_DotsElt(int i, SEXP env);
R_DotType_t R_GetDotType(int i, SEXP env);
SEXP R_DotDelayedExpression(int i, SEXP env);
SEXP R_DotDelayedEnvironment(int i, SEXP env);
SEXP R_DotForcedExpression(int i, SEXP env);
SEXP R_envSymbols(SEXP env);
#endif
SEXP Rf_topenv(SEXP target, SEXP env);
SEXP R_NamespaceEnvSpec(SEXP rho);
SEXP R_PackageEnvName(SEXP rho);
#if R_VERSION >= R_Version(4, 6, 0)
SEXP R_getRegisteredNamespace(const char *name);
#endif
SEXP Rf_GetOption(SEXP tag, SEXP rho);
int Rf_GetOptionWidth(void);
int Rf_GetOptionDigits(void);
SEXP R_GetCurrentEnv(void);
SEXP R_GetCurrentSrcref(int skip);
SEXP R_GetSrcFilename(SEXP srcref);
SEXP R_ClosureExpr(SEXP f);
SEXP R_BytecodeExpr(SEXP e);
#define BODY_EXPR(e) R_ClosureExpr(e)
SEXP (CAD5R)(SEXP e);
LibExtern SEXP R_AtsignSymbol;

/* Evaluation entry points beyond Rf_eval. */
SEXP R_ParseString(const char *text);
SEXP R_ParseEvalString(const char *text, SEXP env);
void Rf_PrintValue(SEXP x);
SEXP R_forceAndCall(SEXP call, int n, SEXP rho);
SEXP R_withCallingErrorHandler(SEXP (*body)(void *), void *bdata,
                               SEXP (*handler)(SEXP, void *), void *hdata);
void R_dot_Last(void);
void R_RunExitFinalizers(void);

/* Classes and S4. */
#if R_VERSION >= R_Version(4, 6, 0)
SEXP R_data_class(SEXP x, Rboolean single);
SEXP R_class(SEXP x);
#endif
SEXP R_do_slot(SEXP obj, SEXP name);
SEXP R_do_slot_assign(SEXP obj, SEXP name, SEXP value);
int R_has_slot(SEXP obj, SEXP name);
SEXP R_do_MAKE_CLASS(const char *what);
SEXP R_getClassDef(const char *what);
SEXP R_do_new_object(SEXP class_def);
int R_check_class_etc(SEXP x, const char **valid);
SEXP Rf_asS4(SEXP x, Rboolean flag, int complete);

/* Attribute queries. */
#if R_VERSION >= R_Version(4, 6, 0)
R_xlen_t R_getAttribCount(SEXP x);
SEXP R_getAttribNames(SEXP x);
SEXP R_getAttributes(SEXP x);
#endif
#ifndef __cplusplus
#include <stdbool.h>
#endif
#if R_VERSION >= R_Version(4, 6, 0)
bool R_hasAttrib(SEXP x, SEXP name);
R_xlen_t R_nrow(SEXP x);
R_xlen_t R_ncol(SEXP x);
#endif
int Rf_nlevels(SEXP f);
SEXP Rf_GetColNames(SEXP dimnames);
SEXP Rf_GetArrayDimnames(SEXP x);
void Rf_GetMatrixDimnames(SEXP x, SEXP *rl, SEXP *cl, const char **rn, const char **cn);

/* Vector utilities. */
SEXP Rf_duplicated(SEXP x, Rboolean from_last);
R_xlen_t Rf_any_duplicated(SEXP x, Rboolean from_last);
R_xlen_t Rf_any_duplicated3(SEXP x, SEXP incomp, Rboolean from_last);
Rcomplex Rf_asComplex(SEXP x);
#if R_VERSION >= R_Version(4, 5, 0)
Rboolean Rf_asRboolean(SEXP x);
bool Rf_asBool(SEXP x);
#endif
#if R_VERSION >= R_Version(4, 4, 0)
Rboolean R_isTRUE(SEXP x);
#endif
void Rf_copyVector(SEXP s, SEXP t);
void Rf_copyMatrix(SEXP s, SEXP t, Rboolean byrow);
void Rf_copyListMatrix(SEXP s, SEXP t, Rboolean byrow);
Rboolean Rf_isVectorizable(SEXP s);
#if R_VERSION >= R_Version(4, 6, 0)
Rboolean Rf_isScalarString(SEXP s);
#endif
Rboolean Rf_isUnsorted(SEXP x, Rboolean strictly);
Rboolean Rf_pmatch(SEXP formal, SEXP tag, Rboolean exact);
Rboolean Rf_psmatch(const char *formal, const char *tag, Rboolean exact);
Rboolean Rf_NonNullStringMatch(SEXP s, SEXP t);
char *Rf_acopy_string(const char *s);
void R_orderVector(int *indx, int n, SEXP arglist, Rboolean nalast, Rboolean decreasing);
void R_orderVector1(int *indx, int n, SEXP x, Rboolean nalast, Rboolean decreasing);
#if R_VERSION >= R_Version(4, 6, 0)
SEXP R_duplicateAsResizable(SEXP x);
#endif
typedef enum { Bytes, Chars, Width } nchar_type;
int R_nchar(SEXP string, nchar_type type, Rboolean allowNA, Rboolean keepNA, const char *msg_name);

/* Hash tables. A table compares keys as identical() does, or by address. */
typedef struct { SEXP cell; } R_hashtab_type;
#define HT_TYPE_IDENTICAL 0
#define HT_TYPE_ADDRESS 1
R_hashtab_type R_mkhashtab(int type, int size);
R_hashtab_type R_asHashtable(SEXP h);
SEXP R_HashtabSEXP(R_hashtab_type h);
int R_isHashtable(SEXP h);
SEXP R_gethash(R_hashtab_type h, SEXP key, SEXP nomatch);
SEXP R_sethash(R_hashtab_type h, SEXP key, SEXP value);
int R_remhash(R_hashtab_type h, SEXP key);
int R_numhash(R_hashtab_type h);
int R_typhash(R_hashtab_type h);
SEXP R_maphash(R_hashtab_type h, SEXP FUN);
SEXP R_maphashC(R_hashtab_type h, SEXP (*FUN)(SEXP, SEXP, void *), void *data);
void R_clrhash(R_hashtab_type h);

/* Names R 4.6 dropped from the API that GNU R 4.3's headers still declared. Package sources
 * pinned to releases from that time (readr, isoband) call them unconditionally, and libR
 * exports them for the binaries built then, so the declarations stay too. */
R_xlen_t (TRUELENGTH)(SEXP x);
R_xlen_t (XTRUELENGTH)(SEXP x);
void (SETLENGTH)(SEXP x, R_xlen_t v);
void (SET_TRUELENGTH)(SEXP x, R_xlen_t v);
int (IS_GROWABLE)(SEXP x);
void (SET_GROWABLE_BIT)(SEXP x);
void (SET_TYPEOF)(SEXP x, SEXPTYPE v);
SEXP (FRAME)(SEXP x);
SEXP (HASHTAB)(SEXP x);
void (SET_ENCLOS)(SEXP x, SEXP v);
void (SET_FORMALS)(SEXP x, SEXP v);
void (SET_BODY)(SEXP x, SEXP v);
void (SET_CLOENV)(SEXP x, SEXP v);
int (RDEBUG)(SEXP x);
void (SET_RDEBUG)(SEXP x, int v);
int (RSTEP)(SEXP x);
void (SET_RSTEP)(SEXP x, int v);
int (RTRACE)(SEXP x);
void (SET_RTRACE)(SEXP x, int v);
int (PRSEEN)(SEXP x);
void (SET_PRSEEN)(SEXP x, int v);
SEXP (SYMVALUE)(SEXP x);
SEXP (INTERNAL)(SEXP x);
int (DDVAL)(SEXP x);
void (SET_PRINTNAME)(SEXP x, SEXP v);
void *(EXTPTR_PTR)(SEXP x);
SEXP (EXTPTR_PROT)(SEXP x);
SEXP (EXTPTR_TAG)(SEXP x);
Rboolean Rf_isBasicClass(const char *name);
SEXP Rf_S3Class(SEXP x);
int Rf_envlength(SEXP rho);
R_xlen_t Rf_envxlength(SEXP rho);
SEXP Rf_CreateTag(SEXP x);
SEXP Rf_type2rstr(SEXPTYPE t);
SEXP Rf_substitute(SEXP lang, SEXP rho);
SEXP R_duplicate_attr(SEXP x);
SEXP R_shallow_duplicate_attr(SEXP x);
SEXP R_body_no_src(SEXP x);

/* The rest of GNU R 4.5's Rinternals.h function list: S4 class queries the methods package
 * answers, binding and frame access for serializers that rebuild environments (qs), XDR
 * encoding, and small predicates. */
SEXP R_FindPackageEnv(SEXP info);
Rboolean R_HasFancyBindings(SEXP rho);
void R_XDREncodeDouble(double d, void *buf);
double R_XDRDecodeDouble(void *buf);
void R_XDREncodeInteger(int i, void *buf);
int R_XDRDecodeInteger(void *buf);
SEXP R_S4_extends(SEXP klass, SEXP useTable);
SEXP R_getClassDef_R(SEXP what);
Rboolean R_has_methods_attached(void);
Rboolean R_isVirtualClass(SEXP class_def, SEXP env);
Rboolean R_extends(SEXP class1, SEXP class2, SEXP env);
int R_check_class_and_super(SEXP x, const char **valid, SEXP rho);
int R_system(const char *command);
const char *R_typeToChar(SEXP x);
Rboolean Rf_conformable(SEXP x, SEXP y);
void Rf_gsetVar(SEXP symbol, SEXP value, SEXP rho);
SEXP Rf_installNoTrChar(SEXP charSXP);
Rboolean Rf_isUserBinop(SEXP s);
int Rf_stringPositionTr(SEXP string, const char *translatedElement);
void SET_FRAME(SEXP x, SEXP v);
void SET_HASHTAB(SEXP x, SEXP v);
#define R_XDR_DOUBLE_SIZE 8
#define R_XDR_INTEGER_SIZE 4

/* Environment flags, which GNU R exposes as functions. */
int (ENVFLAGS)(SEXP x);
void (SET_ENVFLAGS)(SEXP x, int v);

#ifndef R_NO_REMAP
#define acopy_string Rf_acopy_string
#define any_duplicated Rf_any_duplicated
#define any_duplicated3 Rf_any_duplicated3
#define asBool Rf_asBool
#define asComplex Rf_asComplex
#define asRboolean Rf_asRboolean
#define asS4 Rf_asS4
#define CreateTag Rf_CreateTag
#define envlength Rf_envlength
#define envxlength Rf_envxlength
#define isBasicClass Rf_isBasicClass
#define S3Class Rf_S3Class
#define type2rstr Rf_type2rstr
#define substitute Rf_substitute
#define conformable Rf_conformable
#define gsetVar Rf_gsetVar
#define installNoTrChar Rf_installNoTrChar
#define isUserBinop Rf_isUserBinop
#define stringPositionTr Rf_stringPositionTr
#define copyListMatrix Rf_copyListMatrix
#define copyMatrix Rf_copyMatrix
#define copyVector Rf_copyVector
#define duplicated Rf_duplicated
#define GetArrayDimnames Rf_GetArrayDimnames
#define GetColNames Rf_GetColNames
#define GetMatrixDimnames Rf_GetMatrixDimnames
#define GetOption Rf_GetOption
#define GetOptionDigits Rf_GetOptionDigits
#define GetOptionWidth Rf_GetOptionWidth
#define isScalarString Rf_isScalarString
#define isUnsorted Rf_isUnsorted
#define isVectorizable Rf_isVectorizable
#define nlevels Rf_nlevels
#define NonNullStringMatch Rf_NonNullStringMatch
#define pmatch Rf_pmatch
#define psmatch Rf_psmatch
#define PrintValue Rf_PrintValue
#define topenv Rf_topenv
#endif
/* Serialization streams. R_Serialize writes an object through a stream's byte callbacks and
 * R_Unserialize reads one back; the persistence hooks let the caller replace environments,
 * external pointers and weak references by names on the way out and restore them on the way
 * in. The two structs are part of the ABI: packages allocate them and fill their fields. */
typedef void *R_pstream_data_t;

typedef enum {
    R_pstream_any_format,
    R_pstream_ascii_format,
    R_pstream_binary_format,
    R_pstream_xdr_format,
    R_pstream_asciihex_format
} R_pstream_format_t;

typedef struct R_outpstream_st *R_outpstream_t;
struct R_outpstream_st {
    R_pstream_data_t data;
    R_pstream_format_t type;
    int version;
    void (*OutChar)(R_outpstream_t stream, int c);
    void (*OutBytes)(R_outpstream_t stream, void *buf, int length);
    SEXP (*OutPersistHookFunc)(SEXP object, SEXP data);
    SEXP OutPersistHookData;
};

/* The longest encoding name an input stream records. */
#define R_CODESET_MAX 63

typedef struct R_inpstream_st *R_inpstream_t;
struct R_inpstream_st {
    R_pstream_data_t data;
    R_pstream_format_t type;
    int (*InChar)(R_inpstream_t stream);
    void (*InBytes)(R_inpstream_t stream, void *buf, int length);
    SEXP (*InPersistHookFunc)(SEXP name, SEXP data);
    SEXP InPersistHookData;
    char native_encoding[R_CODESET_MAX + 1];
    void *nat2nat_obj;
    void *nat2utf8_obj;
};

void R_InitOutPStream(R_outpstream_t stream, R_pstream_data_t data, R_pstream_format_t type,
                      int version, void (*outchar)(R_outpstream_t, int),
                      void (*outbytes)(R_outpstream_t, void *, int),
                      SEXP (*phook)(SEXP, SEXP), SEXP pdata);
void R_InitInPStream(R_inpstream_t stream, R_pstream_data_t data, R_pstream_format_t type,
                     int (*inchar)(R_inpstream_t), void (*inbytes)(R_inpstream_t, void *, int),
                     SEXP (*phook)(SEXP, SEXP), SEXP pdata);
#ifdef __cplusplus
void R_InitFileOutPStream(R_outpstream_t stream, std::FILE *fp, R_pstream_format_t type,
                          int version, SEXP (*phook)(SEXP, SEXP), SEXP pdata);
void R_InitFileInPStream(R_inpstream_t stream, std::FILE *fp, R_pstream_format_t type,
                         SEXP (*phook)(SEXP, SEXP), SEXP pdata);
#else
void R_InitFileOutPStream(R_outpstream_t stream, FILE *fp, R_pstream_format_t type,
                          int version, SEXP (*phook)(SEXP, SEXP), SEXP pdata);
void R_InitFileInPStream(R_inpstream_t stream, FILE *fp, R_pstream_format_t type,
                         SEXP (*phook)(SEXP, SEXP), SEXP pdata);
#endif
void R_Serialize(SEXP s, R_outpstream_t stream);
SEXP R_Unserialize(R_inpstream_t stream);
/* End of serialization streams. */

#ifndef R_NO_REMAP
#define alloc3DArray Rf_alloc3DArray
#define allocArray Rf_allocArray
#define allocLang Rf_allocLang
#define allocList Rf_allocList
#define allocMatrix Rf_allocMatrix
#define allocS4Object Rf_allocS4Object
#define allocSExp Rf_allocSExp
#define allocVector Rf_allocVector
#define allocVector3 Rf_allocVector3
#define asChar Rf_asChar
#define asCharacterFactor Rf_asCharacterFactor
#define asInteger Rf_asInteger
#define asLogical Rf_asLogical
#define asLogical2 Rf_asLogical2
#define asReal Rf_asReal
#define asXLength Rf_asXLength
#define charIsASCII Rf_charIsASCII
#define charIsLatin1 Rf_charIsLatin1
#define charIsUTF8 Rf_charIsUTF8
#define classgets Rf_classgets
#define coerceVector Rf_coerceVector
#define cons Rf_cons
#define copyMostAttrib Rf_copyMostAttrib
#define defineVar Rf_defineVar
#define dimgets Rf_dimgets
#define dimnamesgets Rf_dimnamesgets
#define duplicate Rf_duplicate
#define elt Rf_elt
#define errorcall Rf_errorcall
#define eval Rf_eval
#define findFun Rf_findFun
#define findVar Rf_findVar
#define findVarInFrame Rf_findVarInFrame
#define findVarInFrame3 Rf_findVarInFrame3
#define getAttrib Rf_getAttrib
#define getCharCE Rf_getCharCE
#define GetOption1 Rf_GetOption1
#define GetRowNames Rf_GetRowNames
#define inherits Rf_inherits
#define install Rf_install
#define installChar Rf_installChar
#define installTrChar Rf_installTrChar
#define isArray Rf_isArray
#define isComplex Rf_isComplex
#define isDataFrame Rf_isDataFrame
#define isEnvironment Rf_isEnvironment
#define isExpression Rf_isExpression
#define isFactor Rf_isFactor
#define isFrame Rf_isFrame
#define isFunction Rf_isFunction
#define isInteger Rf_isInteger
#define isLanguage Rf_isLanguage
#define isList Rf_isList
#define isLogical Rf_isLogical
#define isMatrix Rf_isMatrix
#define isNewList Rf_isNewList
#define isNull Rf_isNull
#define isNumber Rf_isNumber
#define isNumeric Rf_isNumeric
#define isObject Rf_isObject
#define isOrdered Rf_isOrdered
#define isPairList Rf_isPairList
#define isPrimitive Rf_isPrimitive
#define isReal Rf_isReal
#define isS4 Rf_isS4
#define isString Rf_isString
#define isSymbol Rf_isSymbol
#define isTs Rf_isTs
#define isUnordered Rf_isUnordered
#define isValidString Rf_isValidString
#define isValidStringF Rf_isValidStringF
#define isVector Rf_isVector
#define isVectorAtomic Rf_isVectorAtomic
#define isVectorList Rf_isVectorList
#define lang1 Rf_lang1
#define lang2 Rf_lang2
#define lang3 Rf_lang3
#define lang4 Rf_lang4
#define lang5 Rf_lang5
#define lang6 Rf_lang6
#define lastElt Rf_lastElt
#define lazy_duplicate Rf_lazy_duplicate
#define lcons Rf_lcons
#define length(x) Rf_length(x)
#define lengthgets Rf_lengthgets
#define list1 Rf_list1
#define list2 Rf_list2
#define list3 Rf_list3
#define list4 Rf_list4
#define list5 Rf_list5
#define list6 Rf_list6
#define listAppend Rf_listAppend
#define match Rf_match
#define mkChar Rf_mkChar
#define mkCharCE Rf_mkCharCE
#define mkCharLen Rf_mkCharLen
#define mkCharLenCE Rf_mkCharLenCE
#define mkNamed Rf_mkNamed
#define mkString Rf_mkString
#define namesgets Rf_namesgets
#define ncols Rf_ncols
#define nrows Rf_nrows
#define nthcdr Rf_nthcdr
#define onintr Rf_onintr
#define PairToVectorList Rf_PairToVectorList
#define protect Rf_protect
#define reEnc Rf_reEnc
#define ScalarComplex Rf_ScalarComplex
#define ScalarInteger Rf_ScalarInteger
#define ScalarLogical Rf_ScalarLogical
#define ScalarRaw Rf_ScalarRaw
#define ScalarReal Rf_ScalarReal
#define ScalarString Rf_ScalarString
#define setAttrib Rf_setAttrib
#define setVar Rf_setVar
#define shallow_duplicate Rf_shallow_duplicate
#define str2type Rf_str2type
#define StringBlank Rf_StringBlank
#define translateChar Rf_translateChar
#define translateCharFP Rf_translateCharFP
#define translateCharUTF8 Rf_translateCharUTF8
#define type2char Rf_type2char
#define type2str Rf_type2str
#define type2str_nowarn Rf_type2str_nowarn
#define unprotect Rf_unprotect
#define unprotect_ptr Rf_unprotect_ptr
#define VectorToPairList Rf_VectorToPairList
#define warningcall Rf_warningcall
#define warningcall_immediate Rf_warningcall_immediate
#define xlength(x) Rf_xlength(x)
#define xlengthgets Rf_xlengthgets
#endif

#ifdef __cplusplus
}
#endif

#endif
