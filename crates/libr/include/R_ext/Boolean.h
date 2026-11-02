/*
 * SPDX-License-Identifier: LGPL-2.1-or-later
 *
 * Copyright (C) 2025-2026 Felix Andreas. This header is part of ry. It is free software: you
 * can redistribute it and/or modify it under the terms of the GNU Lesser General Public License
 * as published by the Free Software Foundation, either version 2.1 of the License, or (at your
 * option) any later version. See LICENSE at the root of ry's repository.
 */
#ifndef R_EXT_BOOLEAN_H_
#define R_EXT_BOOLEAN_H_

#undef FALSE
#undef TRUE

#ifdef __cplusplus
extern "C" {
#endif

typedef enum { FALSE = 0, TRUE } Rboolean;

#ifdef __cplusplus
}
#endif

#endif
