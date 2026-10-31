/* ALTREP, alternative representations of vectors.
 *
 * A package registers a class for one vector type, then fills in the methods it implements;
 * every method left unset keeps the default behavior. An instance carries two data slots and
 * its class, and every accessor that reaches vector contents (LENGTH, INTEGER, STRING_ELT and
 * the rest) runs the class's methods. ry's implementation is crates/libr/src/altrep.rs.
 */
#ifndef R_EXT_ALTREP_H_
#define R_EXT_ALTREP_H_

#include <Rinternals.h>
#include <R_ext/Rdynload.h>

#ifdef __cplusplus
extern "C" {
#endif

/* A class handle is a one-member struct, passed and returned by value. */
#define STRUCT_SUBTYPES
#define R_SUBTYPE_INIT(x) { x }
typedef struct { SEXP ptr; } R_altrep_class_t;

/* Instances and classes. */
SEXP R_new_altrep(R_altrep_class_t aclass, SEXP data1, SEXP data2);
R_altrep_class_t R_make_altstring_class(const char *cname, const char *pname, DllInfo *info);
R_altrep_class_t R_make_altinteger_class(const char *cname, const char *pname, DllInfo *info);
R_altrep_class_t R_make_altreal_class(const char *cname, const char *pname, DllInfo *info);
R_altrep_class_t R_make_altlogical_class(const char *cname, const char *pname, DllInfo *info);
R_altrep_class_t R_make_altraw_class(const char *cname, const char *pname, DllInfo *info);
R_altrep_class_t R_make_altcomplex_class(const char *cname, const char *pname, DllInfo *info);
R_altrep_class_t R_make_altlist_class(const char *cname, const char *pname, DllInfo *info);
Rboolean R_altrep_inherits(SEXP x, R_altrep_class_t aclass);

/* Methods every class may define. */
typedef SEXP (*R_altrep_UnserializeEX_method_t)(SEXP aclass, SEXP state, SEXP attr, int objf,
                                                int levs);
typedef SEXP (*R_altrep_Unserialize_method_t)(SEXP aclass, SEXP state);
typedef SEXP (*R_altrep_Serialized_state_method_t)(SEXP x);
typedef SEXP (*R_altrep_DuplicateEX_method_t)(SEXP x, Rboolean deep);
typedef SEXP (*R_altrep_Duplicate_method_t)(SEXP x, Rboolean deep);
typedef SEXP (*R_altrep_Coerce_method_t)(SEXP x, int type);
typedef Rboolean (*R_altrep_Inspect_method_t)(SEXP x, int pre, int deep, int pvec,
                                              void (*inspect_subtree)(SEXP, int, int, int));
typedef R_xlen_t (*R_altrep_Length_method_t)(SEXP x);

/* Methods of every vector class. */
typedef void *(*R_altvec_Dataptr_method_t)(SEXP x, Rboolean writeable);
typedef const void *(*R_altvec_Dataptr_or_null_method_t)(SEXP x);
typedef SEXP (*R_altvec_Extract_subset_method_t)(SEXP x, SEXP indx, SEXP call);

/* Methods by vector type. */
typedef int (*R_altinteger_Elt_method_t)(SEXP x, R_xlen_t i);
typedef R_xlen_t (*R_altinteger_Get_region_method_t)(SEXP x, R_xlen_t i, R_xlen_t n, int *buf);
typedef int (*R_altinteger_Is_sorted_method_t)(SEXP x);
typedef int (*R_altinteger_No_NA_method_t)(SEXP x);
typedef SEXP (*R_altinteger_Sum_method_t)(SEXP x, Rboolean narm);
typedef SEXP (*R_altinteger_Min_method_t)(SEXP x, Rboolean narm);
typedef SEXP (*R_altinteger_Max_method_t)(SEXP x, Rboolean narm);

typedef double (*R_altreal_Elt_method_t)(SEXP x, R_xlen_t i);
typedef R_xlen_t (*R_altreal_Get_region_method_t)(SEXP x, R_xlen_t i, R_xlen_t n, double *buf);
typedef int (*R_altreal_Is_sorted_method_t)(SEXP x);
typedef int (*R_altreal_No_NA_method_t)(SEXP x);
typedef SEXP (*R_altreal_Sum_method_t)(SEXP x, Rboolean narm);
typedef SEXP (*R_altreal_Min_method_t)(SEXP x, Rboolean narm);
typedef SEXP (*R_altreal_Max_method_t)(SEXP x, Rboolean narm);

typedef int (*R_altlogical_Elt_method_t)(SEXP x, R_xlen_t i);
typedef R_xlen_t (*R_altlogical_Get_region_method_t)(SEXP x, R_xlen_t i, R_xlen_t n, int *buf);
typedef int (*R_altlogical_Is_sorted_method_t)(SEXP x);
typedef int (*R_altlogical_No_NA_method_t)(SEXP x);
typedef SEXP (*R_altlogical_Sum_method_t)(SEXP x, Rboolean narm);

typedef Rbyte (*R_altraw_Elt_method_t)(SEXP x, R_xlen_t i);
typedef R_xlen_t (*R_altraw_Get_region_method_t)(SEXP x, R_xlen_t i, R_xlen_t n, Rbyte *buf);

typedef Rcomplex (*R_altcomplex_Elt_method_t)(SEXP x, R_xlen_t i);
typedef R_xlen_t (*R_altcomplex_Get_region_method_t)(SEXP x, R_xlen_t i, R_xlen_t n,
                                                     Rcomplex *buf);

typedef SEXP (*R_altstring_Elt_method_t)(SEXP x, R_xlen_t i);
typedef void (*R_altstring_Set_elt_method_t)(SEXP x, R_xlen_t i, SEXP v);
typedef int (*R_altstring_Is_sorted_method_t)(SEXP x);
typedef int (*R_altstring_No_NA_method_t)(SEXP x);

typedef SEXP (*R_altlist_Elt_method_t)(SEXP x, R_xlen_t i);
typedef void (*R_altlist_Set_elt_method_t)(SEXP x, R_xlen_t i, SEXP v);

/* One setter per method: R_set_<family>_<Method>_method(cls, fun). */
#define R_ALTREP_SETTER(FAMILY, METHOD)                                                       \
    void R_set_##FAMILY##_##METHOD##_method(R_altrep_class_t cls,                            \
                                           R_##FAMILY##_##METHOD##_method_t fun)

R_ALTREP_SETTER(altrep, UnserializeEX);
R_ALTREP_SETTER(altrep, Unserialize);
R_ALTREP_SETTER(altrep, Serialized_state);
R_ALTREP_SETTER(altrep, DuplicateEX);
R_ALTREP_SETTER(altrep, Duplicate);
R_ALTREP_SETTER(altrep, Coerce);
R_ALTREP_SETTER(altrep, Inspect);
R_ALTREP_SETTER(altrep, Length);

R_ALTREP_SETTER(altvec, Dataptr);
R_ALTREP_SETTER(altvec, Dataptr_or_null);
R_ALTREP_SETTER(altvec, Extract_subset);

R_ALTREP_SETTER(altinteger, Elt);
R_ALTREP_SETTER(altinteger, Get_region);
R_ALTREP_SETTER(altinteger, Is_sorted);
R_ALTREP_SETTER(altinteger, No_NA);
R_ALTREP_SETTER(altinteger, Sum);
R_ALTREP_SETTER(altinteger, Min);
R_ALTREP_SETTER(altinteger, Max);

R_ALTREP_SETTER(altreal, Elt);
R_ALTREP_SETTER(altreal, Get_region);
R_ALTREP_SETTER(altreal, Is_sorted);
R_ALTREP_SETTER(altreal, No_NA);
R_ALTREP_SETTER(altreal, Sum);
R_ALTREP_SETTER(altreal, Min);
R_ALTREP_SETTER(altreal, Max);

R_ALTREP_SETTER(altlogical, Elt);
R_ALTREP_SETTER(altlogical, Get_region);
R_ALTREP_SETTER(altlogical, Is_sorted);
R_ALTREP_SETTER(altlogical, No_NA);
R_ALTREP_SETTER(altlogical, Sum);

R_ALTREP_SETTER(altraw, Elt);
R_ALTREP_SETTER(altraw, Get_region);

R_ALTREP_SETTER(altcomplex, Elt);
R_ALTREP_SETTER(altcomplex, Get_region);

R_ALTREP_SETTER(altstring, Elt);
R_ALTREP_SETTER(altstring, Set_elt);
R_ALTREP_SETTER(altstring, Is_sorted);
R_ALTREP_SETTER(altstring, No_NA);

R_ALTREP_SETTER(altlist, Elt);
R_ALTREP_SETTER(altlist, Set_elt);

#undef R_ALTREP_SETTER

/* The writable data pointer, for use inside Dataptr methods. */
void *DATAPTR_RW(SEXP x);

#ifdef __cplusplus
}
#endif

#endif
