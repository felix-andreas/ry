#ifndef R_EXT_DYNLOAD_H_
#define R_EXT_DYNLOAD_H_

#include <R_ext/Boolean.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef void *(*DL_FUNC)(void);

typedef unsigned int R_NativePrimitiveArgType;

typedef struct {
    const char *name;
    DL_FUNC fun;
    int numArgs;
    R_NativePrimitiveArgType *types;
} R_CMethodDef;

typedef R_CMethodDef R_FortranMethodDef;

typedef struct {
    const char *name;
    DL_FUNC fun;
    int numArgs;
} R_CallMethodDef;

typedef R_CallMethodDef R_ExternalMethodDef;

/* Opaque, as in GNU R: packages only pass it back to the registration functions. */
typedef struct _DllInfo DllInfo;

int R_registerRoutines(DllInfo *info, const R_CMethodDef *const croutines,
                       const R_CallMethodDef *const callRoutines,
                       const R_FortranMethodDef *const fortranRoutines,
                       const R_ExternalMethodDef *const externalRoutines);

Rboolean R_useDynamicSymbols(DllInfo *info, Rboolean value);
Rboolean R_forceSymbols(DllInfo *info, Rboolean value);

DllInfo *R_getDllInfo(const char *name);
DllInfo *R_getEmbeddingDllInfo(void);

typedef struct Rf_RegisteredNativeSymbol R_RegisteredNativeSymbol;
typedef enum { R_ANY_SYM = 0, R_C_SYM, R_CALL_SYM, R_FORTRAN_SYM, R_EXTERNAL_SYM } NativeSymbolType;

DL_FUNC R_FindSymbol(char const *name, char const *pkg, R_RegisteredNativeSymbol *symbol);

void R_RegisterCCallable(const char *package, const char *name, DL_FUNC fptr);
DL_FUNC R_GetCCallable(const char *package, const char *name);

#ifdef __cplusplus
}
#endif

#endif
