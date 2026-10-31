/* The ALTREP class interface. A class is a table of methods that decide an object's length, its
   elements and its data pointer; ry's dispatch is in crates/libr/src/altrep.rs. */
#ifndef R_EXT_ALTREP_H_
#define R_EXT_ALTREP_H_

#include <Rinternals.h>

#ifdef __cplusplus
extern "C" {
#endif

#define STRUCT_SUBTYPES

typedef struct { SEXP ptr; } R_altrep_class_t;

#define R_SUBTYPE_INIT(x) { x }

SEXP R_new_altrep(R_altrep_class_t aclass, SEXP data1, SEXP data2);
R_altrep_class_t R_make_altstring_class(const char *cname, const char *pname, DllInfo *info);
R_altrep_class_t R_make_altinteger_class(const char *cname, const char *pname, DllInfo *info);
R_altrep_class_t R_make_altreal_class(const char *cname, const char *pname, DllInfo *info);
R_altrep_class_t R_make_altlogical_class(const char *cname, const char *pname, DllInfo *info);
R_altrep_class_t R_make_altraw_class(const char *cname, const char *pname, DllInfo *info);
R_altrep_class_t R_make_altcomplex_class(const char *cname, const char *pname, DllInfo *info);
R_altrep_class_t R_make_altlist_class(const char *cname, const char *pname, DllInfo *info);
Rboolean R_altrep_inherits(SEXP x, R_altrep_class_t aclass);

SEXP R_altrep_data1(SEXP x);
SEXP R_altrep_data2(SEXP x);
void R_set_altrep_data1(SEXP x, SEXP v);
void R_set_altrep_data2(SEXP x, SEXP v);
SEXP ALTREP_CLASS(SEXP x);

typedef R_xlen_t (*R_altrep_Length_method_t)(SEXP);
typedef SEXP (*R_altrep_Duplicate_method_t)(SEXP, Rboolean);
typedef Rboolean (*R_altrep_Inspect_method_t)(SEXP, int, int, int, void (*)(SEXP, int, int, int));
typedef void *(*R_altvec_Dataptr_method_t)(SEXP, Rboolean);
typedef const void *(*R_altvec_Dataptr_or_null_method_t)(SEXP);
typedef SEXP (*R_altvec_Extract_subset_method_t)(SEXP, SEXP, SEXP);
typedef int (*R_altinteger_Elt_method_t)(SEXP, R_xlen_t);
typedef R_xlen_t (*R_altinteger_Get_region_method_t)(SEXP, R_xlen_t, R_xlen_t, int *);
typedef int (*R_altinteger_Is_sorted_method_t)(SEXP);
typedef int (*R_altinteger_No_NA_method_t)(SEXP);
typedef double (*R_altreal_Elt_method_t)(SEXP, R_xlen_t);
typedef R_xlen_t (*R_altreal_Get_region_method_t)(SEXP, R_xlen_t, R_xlen_t, double *);
typedef int (*R_altreal_Is_sorted_method_t)(SEXP);
typedef int (*R_altreal_No_NA_method_t)(SEXP);
typedef int (*R_altlogical_Elt_method_t)(SEXP, R_xlen_t);
typedef Rbyte (*R_altraw_Elt_method_t)(SEXP, R_xlen_t);
typedef Rcomplex (*R_altcomplex_Elt_method_t)(SEXP, R_xlen_t);
typedef SEXP (*R_altstring_Elt_method_t)(SEXP, R_xlen_t);
typedef void (*R_altstring_Set_elt_method_t)(SEXP, R_xlen_t, SEXP);
typedef int (*R_altstring_Is_sorted_method_t)(SEXP);
typedef int (*R_altstring_No_NA_method_t)(SEXP);
typedef SEXP (*R_altlist_Elt_method_t)(SEXP, R_xlen_t);
typedef void (*R_altlist_Set_elt_method_t)(SEXP, R_xlen_t, SEXP);

void R_set_altrep_Length_method(R_altrep_class_t cls, R_altrep_Length_method_t fun);
void R_set_altrep_Duplicate_method(R_altrep_class_t cls, R_altrep_Duplicate_method_t fun);
void R_set_altrep_Inspect_method(R_altrep_class_t cls, R_altrep_Inspect_method_t fun);
void R_set_altvec_Dataptr_method(R_altrep_class_t cls, R_altvec_Dataptr_method_t fun);
void R_set_altvec_Dataptr_or_null_method(R_altrep_class_t cls, R_altvec_Dataptr_or_null_method_t fun);
void R_set_altvec_Extract_subset_method(R_altrep_class_t cls, R_altvec_Extract_subset_method_t fun);
void R_set_altinteger_Elt_method(R_altrep_class_t cls, R_altinteger_Elt_method_t fun);
void R_set_altinteger_Get_region_method(R_altrep_class_t cls, R_altinteger_Get_region_method_t fun);
void R_set_altinteger_Is_sorted_method(R_altrep_class_t cls, R_altinteger_Is_sorted_method_t fun);
void R_set_altinteger_No_NA_method(R_altrep_class_t cls, R_altinteger_No_NA_method_t fun);
void R_set_altreal_Elt_method(R_altrep_class_t cls, R_altreal_Elt_method_t fun);
void R_set_altreal_Get_region_method(R_altrep_class_t cls, R_altreal_Get_region_method_t fun);
void R_set_altreal_Is_sorted_method(R_altrep_class_t cls, R_altreal_Is_sorted_method_t fun);
void R_set_altreal_No_NA_method(R_altrep_class_t cls, R_altreal_No_NA_method_t fun);
void R_set_altlogical_Elt_method(R_altrep_class_t cls, R_altlogical_Elt_method_t fun);
void R_set_altraw_Elt_method(R_altrep_class_t cls, R_altraw_Elt_method_t fun);
void R_set_altcomplex_Elt_method(R_altrep_class_t cls, R_altcomplex_Elt_method_t fun);
void R_set_altstring_Elt_method(R_altrep_class_t cls, R_altstring_Elt_method_t fun);
void R_set_altstring_Set_elt_method(R_altrep_class_t cls, R_altstring_Set_elt_method_t fun);
void R_set_altstring_Is_sorted_method(R_altrep_class_t cls, R_altstring_Is_sorted_method_t fun);
void R_set_altstring_No_NA_method(R_altrep_class_t cls, R_altstring_No_NA_method_t fun);
void R_set_altlist_Elt_method(R_altrep_class_t cls, R_altlist_Elt_method_t fun);
void R_set_altlist_Set_elt_method(R_altrep_class_t cls, R_altlist_Set_elt_method_t fun);

#ifdef __cplusplus
}
#endif

#endif
