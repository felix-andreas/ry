/*
 * SPDX-License-Identifier: LGPL-2.1-or-later
 *
 * Copyright (C) 2025-2026 Felix Andreas. This header is part of ry. It is free software: you
 * can redistribute it and/or modify it under the terms of the GNU Lesser General Public License
 * as published by the Free Software Foundation, either version 2.1 of the License, or (at your
 * option) any later version. See LICENSE at the root of ry's repository.
 */
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
