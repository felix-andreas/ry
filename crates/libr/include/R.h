/*
 * SPDX-License-Identifier: LGPL-2.1-or-later
 *
 * Copyright (C) 2025-2026 Felix Andreas. This header is part of ry. It is free software: you
 * can redistribute it and/or modify it under the terms of the GNU Lesser General Public License
 * as published by the Free Software Foundation, either version 2.1 of the License, or (at your
 * option) any later version. See LICENSE at the root of ry's repository.
 */
#ifndef R_R_H
#define R_R_H

#ifndef USING_R
#define USING_R
#endif

#ifdef __cplusplus
#include <cstdlib>
#include <cstdio>
#include <climits>
#include <cmath>
#include <cstddef>
#else
#include <stdlib.h>
#include <stdio.h>
#include <limits.h>
#include <math.h>
#include <stddef.h>
#endif

#include <Rconfig.h>
#include <R_ext/Arith.h>
#include <R_ext/Boolean.h>
#include <R_ext/Complex.h>
#include <R_ext/Constants.h>
#include <R_ext/Error.h>
#include <R_ext/Memory.h>
#include <R_ext/Print.h>
#include <R_ext/Random.h>
#include <R_ext/Utils.h>
#include <R_ext/RS.h>

#ifdef __cplusplus
extern "C" {
#endif

void R_FlushConsole(void);

#ifdef __cplusplus
}
#endif

#endif
