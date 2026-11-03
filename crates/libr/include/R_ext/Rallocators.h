/*
 * SPDX-License-Identifier: LGPL-2.1-or-later
 *
 * Copyright (C) 2025-2026 Felix Andreas. This header is part of ry. It is free software: you
 * can redistribute it and/or modify it under the terms of the GNU Lesser General Public License
 * as published by the Free Software Foundation, either version 2.1 of the License, or (at your
 * option) any later version. See LICENSE at the root of ry's repository.
 */
#ifndef R_EXT_RALLOCATORS_H_
#define R_EXT_RALLOCATORS_H_

#include <stddef.h>

struct R_allocator;

typedef void *(*custom_alloc_t)(struct R_allocator *allocator, size_t);
typedef void (*custom_free_t)(struct R_allocator *allocator, void *);

struct R_allocator {
    custom_alloc_t mem_alloc;
    custom_free_t mem_free;
    void *res;
    void *data;
};

typedef struct R_allocator R_allocator_t;

#endif
