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
