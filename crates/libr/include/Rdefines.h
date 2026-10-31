/*
 * ry's version of the S-compatible macros of Rdefines.h, defined on top of Rinternals.h. Old
 * code written for S4-style C interfaces uses these names.
 */
#ifndef R_DEFINES_H_
#define R_DEFINES_H_

#if !defined(R_R_H) && !defined(R_S_H)
#include <R.h>
#endif

#include <Rinternals.h>

#define NULL_USER_OBJECT R_NilValue

#define AS_LOGICAL(x) Rf_coerceVector(x, LGLSXP)
#define AS_INTEGER(x) Rf_coerceVector(x, INTSXP)
#define AS_NUMERIC(x) Rf_coerceVector(x, REALSXP)
#define AS_CHARACTER(x) Rf_coerceVector(x, STRSXP)
#define AS_COMPLEX(x) Rf_coerceVector(x, CPLXSXP)
#define AS_VECTOR(x) Rf_coerceVector(x, VECSXP)
#define AS_LIST(x) Rf_coerceVector(x, VECSXP)
#define AS_RAW(x) Rf_coerceVector(x, RAWSXP)

#ifdef USE_RINTERNALS
#define IS_LOGICAL(x) (TYPEOF(x) == LGLSXP)
#define IS_INTEGER(x) (TYPEOF(x) == INTSXP)
#define IS_NUMERIC(x) (TYPEOF(x) == REALSXP)
#define IS_CHARACTER(x) (TYPEOF(x) == STRSXP)
#define IS_COMPLEX(x) (TYPEOF(x) == CPLXSXP)
#else
#define IS_LOGICAL(x) Rf_isLogical(x)
#define IS_INTEGER(x) Rf_isInteger(x)
#define IS_NUMERIC(x) Rf_isReal(x)
#define IS_CHARACTER(x) Rf_isString(x)
#define IS_COMPLEX(x) Rf_isComplex(x)
#endif
#define IS_VECTOR(x) Rf_isVector(x)
#define IS_LIST(x) IS_VECTOR(x)
#define IS_RAW(x) (TYPEOF(x) == RAWSXP)

#define NEW_LOGICAL(n) Rf_allocVector(LGLSXP, n)
#define NEW_INTEGER(n) Rf_allocVector(INTSXP, n)
#define NEW_NUMERIC(n) Rf_allocVector(REALSXP, n)
#define NEW_CHARACTER(n) Rf_allocVector(STRSXP, n)
#define NEW_COMPLEX(n) Rf_allocVector(CPLXSXP, n)
#define NEW_LIST(n) Rf_allocVector(VECSXP, n)
#define NEW_STRING(n) NEW_CHARACTER(n)
#define NEW_RAW(n) Rf_allocVector(RAWSXP, n)

#define LOGICAL_POINTER(x) LOGICAL(x)
#define INTEGER_POINTER(x) INTEGER(x)
#define NUMERIC_POINTER(x) REAL(x)
#define CHARACTER_POINTER(x) STRING_PTR_RO(x)
#define COMPLEX_POINTER(x) COMPLEX(x)
#define LIST_POINTER(x) VECTOR_PTR_RO(x)
#define RAW_POINTER(x) RAW(x)

#define LOGICAL_DATA(x) (LOGICAL(x))
#define INTEGER_DATA(x) (INTEGER(x))
#define DOUBLE_DATA(x) (REAL(x))
#define NUMERIC_DATA(x) (REAL(x))
#define CHARACTER_DATA(x) (STRING_PTR_RO(x))
#define COMPLEX_DATA(x) (COMPLEX(x))

#define LOGICAL_VALUE(x) Rf_asLogical(x)
#define INTEGER_VALUE(x) Rf_asInteger(x)
#define NUMERIC_VALUE(x) Rf_asReal(x)
#define CHARACTER_VALUE(x) CHAR(Rf_asChar(x))
#define STRING_VALUE(x) CHAR(Rf_asChar(x))
#define LIST_VALUE(x) Rf_error("the 'value' of a list object is not defined")

#define GET_LENGTH(x) Rf_length(x)
#define SET_LENGTH(x, n) (x = Rf_lengthgets(x, n))

#define GET_ATTR(x, what) Rf_getAttrib(x, what)
#define GET_CLASS(x) Rf_getAttrib(x, R_ClassSymbol)
#define GET_DIM(x) Rf_getAttrib(x, R_DimSymbol)
#define GET_DIMNAMES(x) Rf_getAttrib(x, R_DimNamesSymbol)
#define GET_COLNAMES(x) Rf_GetColNames(x)
#define GET_ROWNAMES(x) Rf_GetRowNames(x)
#define GET_LEVELS(x) Rf_getAttrib(x, R_LevelsSymbol)
#define GET_TSP(x) Rf_getAttrib(x, R_TspSymbol)
#define GET_NAMES(x) Rf_getAttrib(x, R_NamesSymbol)
#define SET_ATTR(x, what, n) Rf_setAttrib(x, what, n)
#define SET_CLASS(x, n) Rf_setAttrib(x, R_ClassSymbol, n)
#define SET_DIM(x, n) Rf_setAttrib(x, R_DimSymbol, n)
#define SET_DIMNAMES(x, n) Rf_setAttrib(x, R_DimNamesSymbol, n)
#define SET_LEVELS(x, l) Rf_setAttrib(x, R_LevelsSymbol, l)
#define SET_NAMES(x, n) Rf_setAttrib(x, R_NamesSymbol, n)

#define SET_ELEMENT(x, i, val) SET_VECTOR_ELT(x, i, val)
#define SET_STRING_ELT_LIST SET_VECTOR_ELT

#define COPY_TO_USER_STRING(x) Rf_mkChar(x)
#define CREATE_STRING_VECTOR(x) Rf_mkChar(x)
#define CREATE_FUNCTION_CALL(name, argList) Rf_lcons(name, argList)

#define EVAL(x) Rf_eval(x, R_GlobalEnv)

#define GET_SLOT(x, what) R_do_slot(x, what)
#define SET_SLOT(x, what, value) R_do_slot_assign(x, what, value)
#define MAKE_CLASS(what) R_do_MAKE_CLASS(what)
#define NEW_OBJECT(class_def) R_do_new_object(class_def)
#define NEW(class_def) R_do_new_object(class_def)

#define s_object SEXPREC
#define S_EVALUATOR
#define PROBLEM Rf_error(
#define MESSAGE Rf_warning(
#define ERROR ),Rf_error("")
#define RECOVER(x) ),Rf_error("")
#define WARNING(x) ),Rf_warning("")
#define LAST_PROBLEM ),Rf_error("")
#define WARN WARNING(NULL)

#endif
