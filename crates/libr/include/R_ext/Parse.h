/*
 * ry's declaration of R's parser entry point. Parsing runs in ry's interpreter, which libR
 * reaches through its evaluation hook.
 */
#ifndef R_EXT_PARSE_H_
#define R_EXT_PARSE_H_

#include <Rinternals.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    PARSE_NULL,
    PARSE_OK,
    PARSE_INCOMPLETE,
    PARSE_ERROR,
    PARSE_EOF
} ParseStatus;

SEXP R_ParseVector(SEXP text, int n, ParseStatus *status, SEXP srcfile);

#ifdef __cplusplus
}
#endif

#endif
