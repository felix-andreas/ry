/*
 * SPDX-License-Identifier: LGPL-2.1-or-later
 *
 * Copyright (C) 2025-2026 Felix Andreas. This header is part of ry. It is free software: you
 * can redistribute it and/or modify it under the terms of the GNU Lesser General Public License
 * as published by the Free Software Foundation, either version 2.1 of the License, or (at your
 * option) any later version. See LICENSE at the root of ry's repository.
 */
#ifndef R_EXT_CONSTANTS_H_
#define R_EXT_CONSTANTS_H_

#include <float.h>

#ifndef M_PI
#define M_PI 3.141592653589793238462643383280
#endif

#ifndef STRICT_R_HEADERS
#define PI M_PI
#define SINGLE_EPS FLT_EPSILON
#define SINGLE_BASE FLT_RADIX
#define SINGLE_XMIN FLT_MIN
#define SINGLE_XMAX FLT_MAX
#define DOUBLE_DIGITS DBL_MANT_DIG
#define DOUBLE_EPS DBL_EPSILON
#define DOUBLE_XMAX DBL_MAX
#define DOUBLE_XMIN DBL_MIN
#endif

#endif
