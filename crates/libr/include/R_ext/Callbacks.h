/*
 * ry's declaration of top-level task callbacks: C functions that run after each top-level
 * expression, as addTaskCallback() registers them at the R level.
 */
#ifndef R_CALLBACKS_H
#define R_CALLBACKS_H

#include <Rinternals.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef Rboolean (*R_ToplevelCallback)(SEXP expr, SEXP value, Rboolean succeeded,
                                       Rboolean visible, void *data);

typedef struct _ToplevelCallback R_ToplevelCallbackEl;

struct _ToplevelCallback {
    R_ToplevelCallback cb;
    void *data;
    void (*finalizer)(void *data);
    char *name;
    R_ToplevelCallbackEl *next;
};

Rboolean Rf_removeTaskCallbackByIndex(int id);
Rboolean Rf_removeTaskCallbackByName(const char *name);
R_ToplevelCallbackEl *Rf_addTaskCallback(R_ToplevelCallback cb, void *data,
                                         void (*finalizer)(void *), const char *name, int *pos);
SEXP R_removeTaskCallback(SEXP which);

#ifdef __cplusplus
}
#endif

#endif
