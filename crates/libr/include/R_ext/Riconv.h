/*
 * SPDX-License-Identifier: LGPL-2.1-or-later
 *
 * Copyright (C) 2025-2026 Felix Andreas. This header is part of ry. It is free software: you
 * can redistribute it and/or modify it under the terms of the GNU Lesser General Public License
 * as published by the Free Software Foundation, either version 2.1 of the License, or (at your
 * option) any later version. See LICENSE at the root of ry's repository.
 */
#ifndef R_EXT_RICONV_H_
#define R_EXT_RICONV_H_

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

void *Riconv_open(const char *tocode, const char *fromcode);
size_t Riconv(void *cd, const char **inbuf, size_t *inbytesleft, char **outbuf, size_t *outbytesleft);
int Riconv_close(void *cd);

#ifdef __cplusplus
}
#endif

#endif
