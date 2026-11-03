/*
 * SPDX-License-Identifier: LGPL-2.1-or-later
 *
 * Copyright (C) 2025-2026 Felix Andreas. This header is part of ry. It is free software: you
 * can redistribute it and/or modify it under the terms of the GNU Lesser General Public License
 * as published by the Free Software Foundation, either version 2.1 of the License, or (at your
 * option) any later version. See LICENSE at the root of ry's repository.
 */
#ifndef R_RS_H
#define R_RS_H

#include <string.h>
#include <stddef.h>
#include <R_ext/Error.h>

#ifdef __cplusplus
extern "C" {
#endif

void *R_chk_calloc(size_t, size_t);
void *R_chk_realloc(void *, size_t);
void R_chk_free(void *);
void *R_chk_memcpy(void *, const void *, size_t);
void *R_chk_memset(void *, int, size_t);

#define R_Calloc(n, t) (t *) R_chk_calloc((size_t) (n), sizeof(t))
#define R_Realloc(p, n, t) (t *) R_chk_realloc((void *) (p), (size_t) ((n) * sizeof(t)))
#define R_Free(p) (R_chk_free((void *) (p)), (p) = NULL)

#ifndef STRICT_R_HEADERS
#define Calloc R_Calloc
#define Realloc R_Realloc
#define Free R_Free
#endif

#define Memcpy(p, q, n) memcpy(p, q, (size_t) (n) * sizeof(*p))
#define Memzero(p, n) memset(p, 0, (size_t) (n) * sizeof(*p))

#define CallocCharBuf(n) (char *) R_chk_calloc((size_t) ((n) + 1), sizeof(char))

#define F77_NAME(x) x##_
#define F77_CALL(x) x##_
#define F77_SUB(x) x##_
#define F77_COM(x) x##_
#define F77_COMDECL(x) x##_

#ifdef __cplusplus
}
#endif

#endif
