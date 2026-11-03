/*
 * SPDX-License-Identifier: LGPL-2.1-or-later
 *
 * Copyright (C) 2025-2026 Felix Andreas. This header is part of ry. It is free software: you
 * can redistribute it and/or modify it under the terms of the GNU Lesser General Public License
 * as published by the Free Software Foundation, either version 2.1 of the License, or (at your
 * option) any later version. See LICENSE at the root of ry's repository.
 */
#ifndef R_ARITH_H_
#define R_ARITH_H_

#include <R_ext/libextern.h>

#ifdef __cplusplus
extern "C" {
#endif

LibExtern double R_NaN;
LibExtern double R_PosInf;
LibExtern double R_NegInf;
LibExtern double R_NaReal;
LibExtern int R_NaInt;

#define NA_LOGICAL R_NaInt
#define NA_INTEGER R_NaInt
#define NA_REAL R_NaReal

int R_IsNA(double);
int R_IsNaN(double);
int R_finite(double);

#define ISNA(x) R_IsNA(x)

#ifdef __cplusplus
int R_isnancpp(double);
#define ISNAN(x) R_isnancpp(x)
#else
#define ISNAN(x) (isnan(x) != 0)
#endif

#define R_FINITE(x) R_finite(x)

#ifdef __cplusplus
}
#endif

#endif
