/*
 * SPDX-License-Identifier: LGPL-2.1-or-later
 *
 * Copyright (C) 2025-2026 Felix Andreas. This header is part of ry. It is free software: you
 * can redistribute it and/or modify it under the terms of the GNU Lesser General Public License
 * as published by the Free Software Foundation, either version 2.1 of the License, or (at your
 * option) any later version. See LICENSE at the root of ry's repository.
 */
#ifndef R_EXT_MEMORY_H_
#define R_EXT_MEMORY_H_

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

void *vmaxget(void);
void vmaxset(const void *);

void R_gc(void);
int R_gc_running(void);

char *R_alloc(size_t, int);
long double *R_allocLD(size_t);
char *S_alloc(long, int);
char *S_realloc(char *, long, long, int);

void *R_malloc_gc(size_t);
void *R_calloc_gc(size_t, size_t);
void *R_realloc_gc(void *, size_t);

#ifdef __cplusplus
}
#endif

#endif
