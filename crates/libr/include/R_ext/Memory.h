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
