/*
 * ry's declaration of the start-up actions an embedding front end chooses. ry's own front end
 * is the `ry` binary, so only the save actions are used through libR.
 */
#ifndef R_EXT_RSTARTUP_H_
#define R_EXT_RSTARTUP_H_

#include <R_ext/Boolean.h>

typedef enum {
    SA_NORESTORE,
    SA_RESTORE,
    SA_DEFAULT,
    SA_NOSAVE,
    SA_SAVE,
    SA_SAVEASK,
    SA_SUICIDE
} SA_TYPE;

#endif
