/* ry's entry points for a host that embeds the runtime. Not part of GNU R's API. */
#ifndef RY_EMBED_H
#define RY_EMBED_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    uint64_t minor_collections;
    uint64_t major_collections;
    uint64_t objects_freed;
    uint64_t live_objects;
    uint64_t live_bytes;
} RyGcStats;

int ry_initialize(void);
void ry_set_gc_stress(int enabled);
void ry_collect(int major);
void ry_gc_stats(RyGcStats *stats);
int ry_protect_depth(void);
size_t ry_handle_depth(void);
size_t ry_arena_bytes(void);
int ry_context_depth(void);
int ry_warning_count(void);
const char *ry_warning_message(int index);
void ry_clear_warnings(void);
void ry_set_console(void (*callback)(const char *text, size_t length, int is_error, void *data), void *data);
void ry_set_seed(int seed);

#ifdef __cplusplus
}
#endif

#endif
