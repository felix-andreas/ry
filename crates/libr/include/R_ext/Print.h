/*
 * SPDX-License-Identifier: LGPL-2.1-or-later
 *
 * Copyright (C) 2025-2026 Felix Andreas. This header is part of ry. It is free software: you
 * can redistribute it and/or modify it under the terms of the GNU Lesser General Public License
 * as published by the Free Software Foundation, either version 2.1 of the License, or (at your
 * option) any later version. See LICENSE at the root of ry's repository.
 */
#ifndef R_EXT_PRINT_H_
#define R_EXT_PRINT_H_

#include <stdarg.h>

#ifdef __cplusplus
extern "C" {
#endif

#ifndef R_PRINTF_FORMAT
#define R_PRINTF_FORMAT(M, N) __attribute__((format(printf, M, N)))
#endif

void Rprintf(const char *, ...) R_PRINTF_FORMAT(1, 2);
void REprintf(const char *, ...) R_PRINTF_FORMAT(1, 2);
void Rvprintf(const char *, va_list) R_PRINTF_FORMAT(1, 0);
void REvprintf(const char *, va_list) R_PRINTF_FORMAT(1, 0);

#ifdef __cplusplus
}
#endif

#endif
