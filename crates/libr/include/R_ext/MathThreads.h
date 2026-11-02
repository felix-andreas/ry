/*
 * SPDX-License-Identifier: LGPL-2.1-or-later
 *
 * Copyright (C) 2025-2026 Felix Andreas. This header is part of ry. It is free software: you
 * can redistribute it and/or modify it under the terms of the GNU Lesser General Public License
 * as published by the Free Software Foundation, either version 2.1 of the License, or (at your
 * option) any later version. See LICENSE at the root of ry's repository.
 */
/* ry's declaration of the thread counts R's math routines may use. ry's are single-threaded. */
#ifndef R_EXT_MATHTHREADS_H_
#define R_EXT_MATHTHREADS_H_

#ifdef __cplusplus
extern "C" {
#endif

extern int R_num_math_threads;
extern int R_max_num_math_threads;

#ifdef __cplusplus
}
#endif

#endif
