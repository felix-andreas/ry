/*
 * The C side of ry's native boundary.
 *
 * R signals errors with longjmp, and a longjmp must never skip a Rust frame that still owns
 * something to drop. So every jump lands in a C context set up here: native code that ry calls
 * runs inside a trampoline, and an API function that fails records the error in Rust, returns to
 * a frame that owns nothing, and only then jumps through ry_jump. The variadic API functions live
 * here too, because stable Rust cannot define C variadic functions.
 *
 * Every function defined here that native code may call carries RY_EXPORT; build.rs collects
 * them into the version script that exports them from libR.
 */

#include <setjmp.h>
#include <errno.h>
#include <stdint.h>
#include <sys/select.h>
#include <sys/time.h>
#include <unistd.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <Rinternals.h>
#include <R_ext/Rdynload.h>

#define RY_EXPORT __attribute__((visibility("default")))
#define RY_EXPORT_DATA __attribute__((visibility("default")))

/* Implemented in Rust (crates/libr/src/boundary.rs). */
extern void ry_state_save(size_t *protect, size_t *vmax);
extern void ry_state_restore(size_t protect, size_t vmax);
extern void ry_record_error(SEXP call, const char *message);
extern void ry_record_warning(SEXP call, const char *message, int immediate);
extern void ry_write_console(const char *text, size_t length, int is_error);
extern void ry_unwind_save(SEXP cont);
extern void ry_unwind_restore(SEXP cont);
extern void ry_toplevel_report(void);
extern void ry_discard_pending(void);
extern SEXP ry_pending_condition(SEXP classes);
extern void ry_fatal_without_context(void) __attribute__((noreturn));

typedef struct RyContext {
    jmp_buf buffer;
    struct RyContext *previous;
    size_t protect;
    size_t vmax;
} RyContext;

static RyContext *ry_top_context = NULL;

static void ry_context_push(RyContext *context)
{
    ry_state_save(&context->protect, &context->vmax);
    context->previous = ry_top_context;
    ry_top_context = context;
}

static void ry_context_pop(RyContext *context)
{
    ry_top_context = context->previous;
}

/* After a jump lands: the stacks go back to where they were when the context began. */
static void ry_context_land(RyContext *context)
{
    ry_top_context = context->previous;
    ry_state_restore(context->protect, context->vmax);
}

/* Transfers control to the innermost context. The pending unwind is already recorded in Rust. */
__attribute__((noreturn)) void ry_jump(void)
{
    if (ry_top_context == NULL)
        ry_fatal_without_context();
    longjmp(ry_top_context->buffer, 1);
}

RY_EXPORT int ry_context_depth(void)
{
    int depth = 0;
    for (RyContext *context = ry_top_context; context != NULL; context = context->previous)
        depth++;
    return depth;
}

static SEXP ry_dispatch(DL_FUNC function, int count, SEXP *a)
{
    switch (count) {
    case 0: return ((SEXP (*)(void)) function)();
    case 1: return ((SEXP (*)(SEXP)) function)(a[0]);
    case 2: return ((SEXP (*)(SEXP, SEXP)) function)(a[0], a[1]);
    case 3: return ((SEXP (*)(SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2]);
    case 4: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3]);
    case 5: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4]);
    case 6: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5]);
    case 7: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6]);
    case 8: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7]);
    case 9: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8]);
    case 10: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9]);
    case 11: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10]);
    case 12: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11]);
    case 13: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12]);
    case 14: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13]);
    case 15: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14]);
    case 16: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15]);
    case 17: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16]);
    case 18: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17]);
    case 19: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18]);
    case 20: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19]);
    case 21: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20]);
    case 22: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21]);
    case 23: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22]);
    case 24: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23]);
    case 25: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24]);
    case 26: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25]);
    case 27: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26]);
    case 28: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27]);
    case 29: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28]);
    case 30: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29]);
    case 31: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30]);
    case 32: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31]);
    case 33: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32]);
    case 34: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33]);
    case 35: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34]);
    case 36: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35]);
    case 37: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36]);
    case 38: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37]);
    case 39: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37], a[38]);
    case 40: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37], a[38], a[39]);
    case 41: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37], a[38], a[39], a[40]);
    case 42: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37], a[38], a[39], a[40], a[41]);
    case 43: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37], a[38], a[39], a[40], a[41], a[42]);
    case 44: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37], a[38], a[39], a[40], a[41], a[42], a[43]);
    case 45: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37], a[38], a[39], a[40], a[41], a[42], a[43], a[44]);
    case 46: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37], a[38], a[39], a[40], a[41], a[42], a[43], a[44], a[45]);
    case 47: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37], a[38], a[39], a[40], a[41], a[42], a[43], a[44], a[45], a[46]);
    case 48: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37], a[38], a[39], a[40], a[41], a[42], a[43], a[44], a[45], a[46], a[47]);
    case 49: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37], a[38], a[39], a[40], a[41], a[42], a[43], a[44], a[45], a[46], a[47], a[48]);
    case 50: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37], a[38], a[39], a[40], a[41], a[42], a[43], a[44], a[45], a[46], a[47], a[48], a[49]);
    case 51: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37], a[38], a[39], a[40], a[41], a[42], a[43], a[44], a[45], a[46], a[47], a[48], a[49], a[50]);
    case 52: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37], a[38], a[39], a[40], a[41], a[42], a[43], a[44], a[45], a[46], a[47], a[48], a[49], a[50], a[51]);
    case 53: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37], a[38], a[39], a[40], a[41], a[42], a[43], a[44], a[45], a[46], a[47], a[48], a[49], a[50], a[51], a[52]);
    case 54: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37], a[38], a[39], a[40], a[41], a[42], a[43], a[44], a[45], a[46], a[47], a[48], a[49], a[50], a[51], a[52], a[53]);
    case 55: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37], a[38], a[39], a[40], a[41], a[42], a[43], a[44], a[45], a[46], a[47], a[48], a[49], a[50], a[51], a[52], a[53], a[54]);
    case 56: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37], a[38], a[39], a[40], a[41], a[42], a[43], a[44], a[45], a[46], a[47], a[48], a[49], a[50], a[51], a[52], a[53], a[54], a[55]);
    case 57: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37], a[38], a[39], a[40], a[41], a[42], a[43], a[44], a[45], a[46], a[47], a[48], a[49], a[50], a[51], a[52], a[53], a[54], a[55], a[56]);
    case 58: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37], a[38], a[39], a[40], a[41], a[42], a[43], a[44], a[45], a[46], a[47], a[48], a[49], a[50], a[51], a[52], a[53], a[54], a[55], a[56], a[57]);
    case 59: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37], a[38], a[39], a[40], a[41], a[42], a[43], a[44], a[45], a[46], a[47], a[48], a[49], a[50], a[51], a[52], a[53], a[54], a[55], a[56], a[57], a[58]);
    case 60: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37], a[38], a[39], a[40], a[41], a[42], a[43], a[44], a[45], a[46], a[47], a[48], a[49], a[50], a[51], a[52], a[53], a[54], a[55], a[56], a[57], a[58], a[59]);
    case 61: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37], a[38], a[39], a[40], a[41], a[42], a[43], a[44], a[45], a[46], a[47], a[48], a[49], a[50], a[51], a[52], a[53], a[54], a[55], a[56], a[57], a[58], a[59], a[60]);
    case 62: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37], a[38], a[39], a[40], a[41], a[42], a[43], a[44], a[45], a[46], a[47], a[48], a[49], a[50], a[51], a[52], a[53], a[54], a[55], a[56], a[57], a[58], a[59], a[60], a[61]);
    case 63: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37], a[38], a[39], a[40], a[41], a[42], a[43], a[44], a[45], a[46], a[47], a[48], a[49], a[50], a[51], a[52], a[53], a[54], a[55], a[56], a[57], a[58], a[59], a[60], a[61], a[62]);
    case 64: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37], a[38], a[39], a[40], a[41], a[42], a[43], a[44], a[45], a[46], a[47], a[48], a[49], a[50], a[51], a[52], a[53], a[54], a[55], a[56], a[57], a[58], a[59], a[60], a[61], a[62], a[63]);
    case 65: return ((SEXP (*)(SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP, SEXP)) function)(a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8], a[9], a[10], a[11], a[12], a[13], a[14], a[15], a[16], a[17], a[18], a[19], a[20], a[21], a[22], a[23], a[24], a[25], a[26], a[27], a[28], a[29], a[30], a[31], a[32], a[33], a[34], a[35], a[36], a[37], a[38], a[39], a[40], a[41], a[42], a[43], a[44], a[45], a[46], a[47], a[48], a[49], a[50], a[51], a[52], a[53], a[54], a[55], a[56], a[57], a[58], a[59], a[60], a[61], a[62], a[63], a[64]);
    default: return NULL;
    }
}

/* Runs a .Call or .External routine. Returns 0 with *result set, or 1 after an unwind. */
int ry_call_native(DL_FUNC function, int count, SEXP *args, SEXP *result)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        return 1;
    }
    *result = ry_dispatch(function, count, args);
    ry_context_pop(&context);
    return 0;
}

int ry_call_void(void (*function)(void *), void *data)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        return 1;
    }
    function(data);
    ry_context_pop(&context);
    return 0;
}

int ry_call_init(void (*function)(DllInfo *), DllInfo *info)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        return 1;
    }
    function(info);
    ry_context_pop(&context);
    return 0;
}

int ry_call_finalizer(R_CFinalizer_t function, SEXP object)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        return 1;
    }
    function(object);
    ry_context_pop(&context);
    return 0;
}

/* ALTREP methods are native code too, so each method kind has its trampoline. */

int ry_altrep_length(R_xlen_t (*method)(SEXP), SEXP x, R_xlen_t *result)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        return 1;
    }
    *result = method(x);
    ry_context_pop(&context);
    return 0;
}

int ry_altrep_dataptr(void *(*method)(SEXP, Rboolean), SEXP x, int writable, void **result)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        return 1;
    }
    *result = method(x, writable ? TRUE : FALSE);
    ry_context_pop(&context);
    return 0;
}

int ry_altrep_dataptr_or_null(const void *(*method)(SEXP), SEXP x, const void **result)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        return 1;
    }
    *result = method(x);
    ry_context_pop(&context);
    return 0;
}

int ry_altrep_int_elt(int (*method)(SEXP, R_xlen_t), SEXP x, R_xlen_t i, int *result)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        return 1;
    }
    *result = method(x, i);
    ry_context_pop(&context);
    return 0;
}

int ry_altrep_real_elt(double (*method)(SEXP, R_xlen_t), SEXP x, R_xlen_t i, double *result)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        return 1;
    }
    *result = method(x, i);
    ry_context_pop(&context);
    return 0;
}

int ry_altrep_sexp_elt(SEXP (*method)(SEXP, R_xlen_t), SEXP x, R_xlen_t i, SEXP *result)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        return 1;
    }
    *result = method(x, i);
    ry_context_pop(&context);
    return 0;
}

int ry_altrep_int_query(int (*method)(SEXP), SEXP x, int *result)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        return 1;
    }
    *result = method(x);
    ry_context_pop(&context);
    return 0;
}

int ry_altrep_region(R_xlen_t (*method)(SEXP, R_xlen_t, R_xlen_t, void *), SEXP x, R_xlen_t start,
                     R_xlen_t count, void *buffer, R_xlen_t *result)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        return 1;
    }
    *result = method(x, start, count, buffer);
    ry_context_pop(&context);
    return 0;
}

int ry_altrep_raw_elt(Rbyte (*method)(SEXP, R_xlen_t), SEXP x, R_xlen_t i, Rbyte *result)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        return 1;
    }
    *result = method(x, i);
    ry_context_pop(&context);
    return 0;
}

int ry_altrep_complex_elt(Rcomplex (*method)(SEXP, R_xlen_t), SEXP x, R_xlen_t i, Rcomplex *result)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        return 1;
    }
    *result = method(x, i);
    ry_context_pop(&context);
    return 0;
}

int ry_altrep_set_elt(void (*method)(SEXP, R_xlen_t, SEXP), SEXP x, R_xlen_t i, SEXP v)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        return 1;
    }
    method(x, i, v);
    ry_context_pop(&context);
    return 0;
}

/* Errors, warnings and console output. */

#define RY_BUFFER_SIZE 8192

RY_EXPORT void Rf_error(const char *format, ...)
{
    char buffer[RY_BUFFER_SIZE];
    va_list arguments;
    va_start(arguments, format);
    vsnprintf(buffer, sizeof buffer, format, arguments);
    va_end(arguments);
    ry_record_error(R_NilValue, buffer);
    ry_jump();
}

RY_EXPORT void Rf_errorcall(SEXP call, const char *format, ...)
{
    char buffer[RY_BUFFER_SIZE];
    va_list arguments;
    va_start(arguments, format);
    vsnprintf(buffer, sizeof buffer, format, arguments);
    va_end(arguments);
    ry_record_error(call, buffer);
    ry_jump();
}

RY_EXPORT void Rf_warning(const char *format, ...)
{
    char buffer[RY_BUFFER_SIZE];
    va_list arguments;
    va_start(arguments, format);
    vsnprintf(buffer, sizeof buffer, format, arguments);
    va_end(arguments);
    ry_record_warning(R_NilValue, buffer, 0);
}

RY_EXPORT void Rf_warningcall(SEXP call, const char *format, ...)
{
    char buffer[RY_BUFFER_SIZE];
    va_list arguments;
    va_start(arguments, format);
    vsnprintf(buffer, sizeof buffer, format, arguments);
    va_end(arguments);
    ry_record_warning(call, buffer, 0);
}

RY_EXPORT void Rf_warningcall_immediate(SEXP call, const char *format, ...)
{
    char buffer[RY_BUFFER_SIZE];
    va_list arguments;
    va_start(arguments, format);
    vsnprintf(buffer, sizeof buffer, format, arguments);
    va_end(arguments);
    ry_record_warning(call, buffer, 1);
}

static void ry_vprint(int is_error, const char *format, va_list arguments)
{
    char buffer[RY_BUFFER_SIZE];
    va_list copy;
    va_copy(copy, arguments);
    int length = vsnprintf(buffer, sizeof buffer, format, arguments);
    if (length < 0) {
        va_end(copy);
        return;
    }
    if ((size_t) length < sizeof buffer) {
        ry_write_console(buffer, (size_t) length, is_error);
    } else {
        char *large = malloc((size_t) length + 1);
        if (large != NULL) {
            vsnprintf(large, (size_t) length + 1, format, copy);
            ry_write_console(large, (size_t) length, is_error);
            free(large);
        }
    }
    va_end(copy);
}

RY_EXPORT void Rprintf(const char *format, ...)
{
    va_list arguments;
    va_start(arguments, format);
    ry_vprint(0, format, arguments);
    va_end(arguments);
}

RY_EXPORT void REprintf(const char *format, ...)
{
    va_list arguments;
    va_start(arguments, format);
    ry_vprint(1, format, arguments);
    va_end(arguments);
}

RY_EXPORT void Rvprintf(const char *format, va_list arguments)
{
    ry_vprint(0, format, arguments);
}

RY_EXPORT void REvprintf(const char *format, va_list arguments)
{
    ry_vprint(1, format, arguments);
}

/* Callbacks into native code run from C, so an error inside them never skips a Rust frame. */

RY_EXPORT SEXP R_mapAttrib(SEXP x, SEXP (*FUN)(SEXP, SEXP, void *), void *data)
{
    SEXP attributes = PROTECT(ATTRIB(x));
    for (SEXP cell = attributes; cell != R_NilValue; cell = CDR(cell)) {
        SEXP result = FUN(TAG(cell), CAR(cell), data);
        if (result != NULL) {
            UNPROTECT(1);
            return result;
        }
    }
    UNPROTECT(1);
    return NULL;
}

/* Non-local exits that native code sets up itself. */

/*
 * The body runs in a context. When an unwind reaches it, the unwind moves into `cont`, the
 * cleanup runs with jump = TRUE (C++ bindings throw from it, so their destructors run, and call
 * R_ContinueUnwind later), and if the cleanup returns, the unwind continues outward from here.
 */
RY_EXPORT SEXP R_UnwindProtect(SEXP (*fun)(void *), void *data,
                               void (*cleanfun)(void *, Rboolean), void *cleandata, SEXP cont)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        ry_unwind_save(cont);
        cleanfun(cleandata, TRUE);
        R_ContinueUnwind(cont);
    }
    SEXP result = fun(data);
    ry_context_pop(&context);
    cleanfun(cleandata, FALSE);
    return result;
}

RY_EXPORT void R_ContinueUnwind(SEXP cont)
{
    ry_unwind_restore(cont);
    ry_jump();
}

RY_EXPORT SEXP R_ExecWithCleanup(SEXP (*fun)(void *), void *data, void (*cleanfun)(void *),
                                 void *cleandata)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        cleanfun(cleandata);
        ry_jump();
    }
    SEXP result = fun(data);
    ry_context_pop(&context);
    cleanfun(cleandata);
    return result;
}

/* An error inside is reported and stops at this boundary, which then returns FALSE. */
RY_EXPORT Rboolean R_ToplevelExec(void (*fun)(void *), void *data)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        ry_toplevel_report();
        return FALSE;
    }
    fun(data);
    ry_context_pop(&context);
    return TRUE;
}

/*
 * Errors (class "error") and interrupts (class "interrupt") are the conditions that unwind; the
 * handler runs for one whose class is in `conds`. Warnings and messages do not unwind, so they
 * are never caught here.
 */
RY_EXPORT SEXP R_tryCatch(SEXP (*body)(void *), void *bdata, SEXP conds,
                          SEXP (*handler)(SEXP, void *), void *hdata,
                          void (*finally)(void *), void *fdata)
{
    RyContext context;
    PROTECT(conds);
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        SEXP condition = ry_pending_condition(conds);
        if (condition == NULL) {
            if (finally != NULL)
                finally(fdata);
            ry_jump();
        }
        PROTECT(condition);
        ry_discard_pending();
        /* The handler may raise in turn; `finally` runs on that path too. */
        RyContext handling;
        ry_context_push(&handling);
        if (setjmp(handling.buffer)) {
            ry_context_land(&handling);
            if (finally != NULL)
                finally(fdata);
            ry_jump();
        }
        SEXP result = handler(condition, hdata);
        ry_context_pop(&handling);
        UNPROTECT(2);
        if (finally != NULL)
            finally(fdata);
        return result;
    }
    SEXP result = body(bdata);
    ry_context_pop(&context);
    UNPROTECT(1);
    if (finally != NULL)
        finally(fdata);
    return result;
}

RY_EXPORT SEXP R_tryCatchError(SEXP (*body)(void *), void *bdata,
                               SEXP (*handler)(SEXP, void *), void *hdata)
{
    SEXP conds = PROTECT(Rf_mkString("error"));
    SEXP result = R_tryCatch(body, bdata, conds, handler, hdata, NULL, NULL);
    UNPROTECT(1);
    return result;
}

/*
 * The front-end interface (Rinterface.h, Rembedded.h, R_ext/eventloop.h, R_ext/Callbacks.h).
 * GNU R exports these as data and as hooks a front end replaces; packages such as later share
 * the event loop through them. The variables are plain C data, so they live here.
 */

typedef void (*RyInputHandlerProc)(void *);

typedef struct RyInputHandler {
    int activity;
    int fileDescriptor;
    RyInputHandlerProc handler;
    struct RyInputHandler *next;
    int active;
    void *userData;
} RyInputHandler;

RY_EXPORT_DATA RyInputHandler *R_InputHandlers = NULL;
RY_EXPORT_DATA void (*R_PolledEvents)(void) = NULL;
RY_EXPORT_DATA int R_wait_usec = 0;

RY_EXPORT RyInputHandler *addInputHandler(RyInputHandler *handlers, int fd,
                                          RyInputHandlerProc handler, int activity)
{
    RyInputHandler *input = calloc(1, sizeof *input);
    if (input == NULL)
        return NULL;
    input->activity = activity;
    input->fileDescriptor = fd;
    input->handler = handler;
    input->active = 1;
    if (handlers == NULL) {
        R_InputHandlers = input;
        return input;
    }
    RyInputHandler *last = handlers;
    while (last->next != NULL)
        last = last->next;
    last->next = input;
    return input;
}

RY_EXPORT RyInputHandler *getInputHandler(RyInputHandler *handlers, int fd)
{
    for (RyInputHandler *input = handlers; input != NULL; input = input->next)
        if (input->fileDescriptor == fd)
            return input;
    return NULL;
}

RY_EXPORT int removeInputHandler(RyInputHandler **handlers, RyInputHandler *it)
{
    if (handlers == NULL || it == NULL)
        return 0;
    for (RyInputHandler **link = handlers; *link != NULL; link = &(*link)->next) {
        if (*link == it) {
            *link = it->next;
            free(it);
            return 1;
        }
    }
    return 0;
}

RY_EXPORT RyInputHandler *getSelectedHandler(RyInputHandler *handlers, fd_set *mask)
{
    for (RyInputHandler *input = handlers; input != NULL; input = input->next)
        if (input->active && FD_ISSET(input->fileDescriptor, mask)) {
            FD_CLR(input->fileDescriptor, mask);
            return input;
        }
    return NULL;
}

RY_EXPORT int R_SelectEx(int n, fd_set *readfds, fd_set *writefds, fd_set *exceptfds,
                         struct timeval *timeout, void (*intr)(void))
{
    (void) intr;
    return select(n, readfds, writefds, exceptfds, timeout);
}

/* The descriptors with input pending, waiting at most `usec` microseconds (forever if negative). */
RY_EXPORT fd_set *R_checkActivity(int usec, int ignore_stdin)
{
    static fd_set ready;
    struct timeval wait;
    int highest = -1;
    FD_ZERO(&ready);
    if (!ignore_stdin) {
        FD_SET(STDIN_FILENO, &ready);
        highest = STDIN_FILENO;
    }
    for (RyInputHandler *input = R_InputHandlers; input != NULL; input = input->next) {
        if (input->active && input->fileDescriptor >= 0) {
            FD_SET(input->fileDescriptor, &ready);
            if (input->fileDescriptor > highest)
                highest = input->fileDescriptor;
        }
    }
    if (R_wait_usec > 0 && (usec < 0 || R_wait_usec < usec))
        usec = R_wait_usec;
    wait.tv_sec = usec < 0 ? 0 : usec / 1000000;
    wait.tv_usec = usec < 0 ? 0 : usec % 1000000;
    int found = select(highest + 1, &ready, NULL, NULL, usec < 0 ? NULL : &wait);
    return found > 0 ? &ready : NULL;
}

RY_EXPORT void R_runHandlers(RyInputHandler *handlers, fd_set *mask)
{
    if (mask == NULL) {
        if (R_PolledEvents != NULL)
            R_PolledEvents();
        return;
    }
    RyInputHandler *input = handlers;
    while (input != NULL) {
        RyInputHandler *next = input->next;
        if (input->active && input->fileDescriptor >= 0 && FD_ISSET(input->fileDescriptor, mask))
            input->handler(input->userData);
        input = next;
    }
}

/* R_ProcessEvents runs the polled-events hook and acts on a pending interrupt. */
RY_EXPORT void R_ProcessEvents(void)
{
    if (R_PolledEvents != NULL)
        R_PolledEvents();
    R_CheckUserInterrupt();
}

/* Console and front-end hooks a front end may replace. */

typedef enum { RY_SA_NORESTORE, RY_SA_RESTORE, RY_SA_DEFAULT, RY_SA_NOSAVE, RY_SA_SAVE,
               RY_SA_SAVEASK, RY_SA_SUICIDE } RySaveAction;

RY_EXPORT_DATA char *R_Home = NULL;
RY_EXPORT_DATA char *R_TempDir = NULL;
RY_EXPORT_DATA char *R_GUIType = "unknown";
RY_EXPORT_DATA char *R_HistoryFile = ".Rhistory";
RY_EXPORT_DATA int R_HistorySize = 512;
RY_EXPORT_DATA int R_RestoreHistory = 0;
RY_EXPORT_DATA int R_DirtyImage = 0;
RY_EXPORT_DATA Rboolean R_NoEcho = FALSE;
RY_EXPORT_DATA FILE *R_Consolefile = NULL;
RY_EXPORT_DATA FILE *R_Outputfile = NULL;
RY_EXPORT_DATA int R_running_as_main_program = 0;
RY_EXPORT_DATA int R_SignalHandlers = 0;
RY_EXPORT_DATA int R_isForkedChild = 0;
/* No C stack checking: the limit is the largest address, as when GNU R disables the check. */
RY_EXPORT_DATA uintptr_t R_CStackLimit = (uintptr_t) -1;
RY_EXPORT_DATA uintptr_t R_CStackStart = (uintptr_t) -1;
RY_EXPORT_DATA int R_CStackDir = 1;
RY_EXPORT_DATA int (*R_timeout_handler)(void) = NULL;
RY_EXPORT_DATA long R_timeout_val = 0;
RY_EXPORT_DATA Rboolean UserBreak = FALSE;
RY_EXPORT_DATA int R_num_math_threads = 1;
RY_EXPORT_DATA int R_max_num_math_threads = 1;

/*
 * GNU R's context stack is opaque to packages; later declares R_GlobalContext itself and only
 * compares it with the top-level context, so both point at one zeroed block that reads as "at
 * top level".
 */
static char ry_toplevel_context[1024];
RY_EXPORT_DATA void *R_GlobalContext = ry_toplevel_context;
RY_EXPORT_DATA void *R_ToplevelContext = ry_toplevel_context;

RY_EXPORT_DATA void (*ptr_R_Suicide)(const char *) = NULL;
RY_EXPORT_DATA void (*ptr_R_ShowMessage)(const char *) = NULL;
RY_EXPORT_DATA int (*ptr_R_ReadConsole)(const char *, unsigned char *, int, int) = NULL;
RY_EXPORT_DATA void (*ptr_R_WriteConsole)(const char *, int) = NULL;
RY_EXPORT_DATA void (*ptr_R_WriteConsoleEx)(const char *, int, int) = NULL;
RY_EXPORT_DATA void (*ptr_R_ResetConsole)(void) = NULL;
RY_EXPORT_DATA void (*ptr_R_FlushConsole)(void) = NULL;
RY_EXPORT_DATA void (*ptr_R_ClearerrConsole)(void) = NULL;
RY_EXPORT_DATA void (*ptr_R_Busy)(int) = NULL;
RY_EXPORT_DATA void (*ptr_R_CleanUp)(RySaveAction, int, int) = NULL;
RY_EXPORT_DATA int (*ptr_R_ShowFiles)(int, const char **, const char **, const char *,
                                      Rboolean, const char *) = NULL;
RY_EXPORT_DATA int (*ptr_R_ChooseFile)(int, char *, int) = NULL;
RY_EXPORT_DATA int (*ptr_R_EditFile)(const char *) = NULL;
RY_EXPORT_DATA void (*ptr_R_loadhistory)(SEXP, SEXP, SEXP, SEXP) = NULL;
RY_EXPORT_DATA void (*ptr_R_savehistory)(SEXP, SEXP, SEXP, SEXP) = NULL;
RY_EXPORT_DATA void (*ptr_R_addhistory)(SEXP, SEXP, SEXP, SEXP) = NULL;
RY_EXPORT_DATA int (*ptr_R_EditFiles)(int, const char **, const char **, const char *) = NULL;
RY_EXPORT_DATA SEXP (*ptr_do_selectlist)(SEXP, SEXP, SEXP, SEXP) = NULL;
RY_EXPORT_DATA SEXP (*ptr_do_dataentry)(SEXP, SEXP, SEXP, SEXP) = NULL;
RY_EXPORT_DATA SEXP (*ptr_do_dataviewer)(SEXP, SEXP, SEXP, SEXP) = NULL;
RY_EXPORT_DATA void (*ptr_R_ProcessEvents)(void) = NULL;

/* Called once when the runtime starts: the streams are only known at run time. */
void ry_frontend_start(const char *home, const char *temp_dir)
{
    R_Consolefile = stderr;
    R_Outputfile = stdout;
    if (home != NULL)
        R_Home = strdup(home);
    if (temp_dir != NULL)
        R_TempDir = strdup(temp_dir);
}

RY_EXPORT void R_Busy(int which)
{
    if (ptr_R_Busy != NULL)
        ptr_R_Busy(which);
}

/* Reads a line of console input into buf (len bytes, including the newline); 0 at end of input. */
RY_EXPORT int R_ReadConsole(const char *prompt, unsigned char *buf, int len, int addtohistory)
{
    if (ptr_R_ReadConsole != NULL)
        return ptr_R_ReadConsole(prompt, buf, len, addtohistory);
    if (prompt != NULL) {
        fputs(prompt, stdout);
        fflush(stdout);
    }
    if (len <= 0 || fgets((char *) buf, len, stdin) == NULL)
        return 0;
    return 1;
}

RY_EXPORT void R_WriteConsole(const char *buf, int len)
{
    if (ptr_R_WriteConsole != NULL) {
        ptr_R_WriteConsole(buf, len);
        return;
    }
    ry_write_console(buf, (size_t) len, 0);
}

RY_EXPORT void R_WriteConsoleEx(const char *buf, int len, int otype)
{
    if (ptr_R_WriteConsoleEx != NULL) {
        ptr_R_WriteConsoleEx(buf, len, otype);
        return;
    }
    ry_write_console(buf, (size_t) len, otype != 0);
}

RY_EXPORT void R_ResetConsole(void)
{
    if (ptr_R_ResetConsole != NULL)
        ptr_R_ResetConsole();
}

RY_EXPORT void R_ClearerrConsole(void)
{
    if (ptr_R_ClearerrConsole != NULL)
        ptr_R_ClearerrConsole();
    clearerr(stdin);
}

RY_EXPORT char *R_HomeDir(void)
{
    return R_Home;
}

extern void ry_run_exit_finalizers(void);

RY_EXPORT void R_Suicide(const char *message)
{
    if (ptr_R_Suicide != NULL)
        ptr_R_Suicide(message);
    fprintf(stderr, "Fatal error: %s\n", message);
    exit(2);
}

RY_EXPORT void R_CleanUp(RySaveAction save, int status, int run_last)
{
    (void) save;
    (void) run_last;
    if (ptr_R_CleanUp != NULL)
        ptr_R_CleanUp(save, status, run_last);
    ry_run_exit_finalizers();
    exit(status);
}

RY_EXPORT void R_setStartTime(void) {}

RY_EXPORT void fpu_setup(Rboolean start) { (void) start; }

RY_EXPORT int R_EditFiles(int nfile, const char **file, const char **title, const char *editor)
{
    if (ptr_R_EditFiles != NULL)
        return ptr_R_EditFiles(nfile, file, title, editor);
    return 1;
}

RY_EXPORT void R_CleanTempDir(void)
{
    /* The runtime removes its session directory itself at exit. */
}

RY_EXPORT void R_SaveGlobalEnv(void) {}
RY_EXPORT void R_RestoreGlobalEnv(void) {}
RY_EXPORT void R_SaveGlobalEnvToFile(const char *name) { (void) name; }
RY_EXPORT void R_RestoreGlobalEnvFromFile(const char *name, Rboolean quiet)
{
    (void) name;
    (void) quiet;
}
RY_EXPORT void R_setupHistory(void) {}
RY_EXPORT void CleanEd(void) {}

/* Reading Renviron files is the front end's job; the `ry` binary does it before libR starts. */
RY_EXPORT void process_site_Renviron(void) {}
RY_EXPORT void process_system_Renviron(void) {}
RY_EXPORT void process_user_Renviron(void) {}

/* Abandons the current computation, as an error with an empty message does. */
RY_EXPORT void Rf_jump_to_toplevel(void)
{
    Rf_error("%s", "");
}

/* Task callbacks run after each top-level task; the interpreter calls ry_run_task_callbacks. */

typedef Rboolean (*RyTaskCallback)(SEXP, SEXP, Rboolean, Rboolean, void *);

typedef struct RyToplevelCallback {
    RyTaskCallback cb;
    void *data;
    void (*finalizer)(void *data);
    char *name;
    struct RyToplevelCallback *next;
} RyToplevelCallback;

static RyToplevelCallback *ry_task_callbacks = NULL;

static void ry_free_task_callback(RyToplevelCallback *callback)
{
    if (callback->finalizer != NULL)
        callback->finalizer(callback->data);
    free(callback->name);
    free(callback);
}

RY_EXPORT RyToplevelCallback *Rf_addTaskCallback(RyTaskCallback cb, void *data,
                                                 void (*finalizer)(void *), const char *name,
                                                 int *pos)
{
    RyToplevelCallback *callback = calloc(1, sizeof *callback);
    if (callback == NULL)
        Rf_error("cannot allocate a task callback");
    callback->cb = cb;
    callback->data = data;
    callback->finalizer = finalizer;
    int position = 1;
    RyToplevelCallback **link = &ry_task_callbacks;
    while (*link != NULL) {
        link = &(*link)->next;
        position++;
    }
    *link = callback;
    if (name != NULL) {
        callback->name = strdup(name);
    } else {
        char generated[32];
        snprintf(generated, sizeof generated, "%d", position);
        callback->name = strdup(generated);
    }
    if (pos != NULL)
        *pos = position;
    return callback;
}

RY_EXPORT Rboolean Rf_removeTaskCallbackByIndex(int index)
{
    RyToplevelCallback **link = &ry_task_callbacks;
    for (int position = 1; *link != NULL; position++, link = &(*link)->next) {
        if (position == index) {
            RyToplevelCallback *callback = *link;
            *link = callback->next;
            ry_free_task_callback(callback);
            return TRUE;
        }
    }
    return FALSE;
}

RY_EXPORT Rboolean Rf_removeTaskCallbackByName(const char *name)
{
    for (RyToplevelCallback **link = &ry_task_callbacks; *link != NULL; link = &(*link)->next) {
        if (strcmp((*link)->name, name) == 0) {
            RyToplevelCallback *callback = *link;
            *link = callback->next;
            ry_free_task_callback(callback);
            return TRUE;
        }
    }
    return FALSE;
}

/* Runs every task callback after a top-level task; one that returns FALSE is removed. */
RY_EXPORT void ry_run_task_callbacks(SEXP expr, SEXP value, Rboolean succeeded, Rboolean visible)
{
    RyToplevelCallback **link = &ry_task_callbacks;
    while (*link != NULL) {
        RyToplevelCallback *callback = *link;
        if (!callback->cb(expr, value, succeeded, visible, callback->data)) {
            *link = callback->next;
            ry_free_task_callback(callback);
        } else {
            link = &callback->next;
        }
    }
}

/* R_maphashC's callback is native code; the status is unused because an error jumps instead. */
SEXP ry_maphash_call(SEXP (*function)(SEXP, SEXP, void *), SEXP key, SEXP value, void *data,
                     int *status)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        *status = 1;
        return NULL;
    }
    SEXP result = function(key, value, data);
    ry_context_pop(&context);
    *status = 0;
    return result;
}

/*
 * R_withCallingErrorHandler: the handler sees an error raised in the body and the error then
 * continues outward. GNU R runs a calling handler before the stack unwinds; here the native
 * frames between the error and this context are already gone when the handler runs, which only
 * matters to a handler that inspects them.
 */
RY_EXPORT SEXP R_withCallingErrorHandler(SEXP (*body)(void *), void *bdata,
                                         SEXP (*handler)(SEXP, void *), void *hdata)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        SEXP classes = PROTECT(Rf_mkString("error"));
        SEXP condition = ry_pending_condition(classes);
        if (condition != NULL) {
            PROTECT(condition);
            handler(condition, hdata);
            UNPROTECT(1);
        }
        UNPROTECT(1);
        ry_jump();
    }
    SEXP result = body(bdata);
    ry_context_pop(&context);
    return result;
}
/* ALTREP methods with SEXP results (crates/libr/src/altrep.rs). A method may return C NULL,
   which means "use the default", so the result is passed back as a raw pointer. */

#define RY_ALTREP_TRAMPOLINE(name, parameters, call)                                         \
    int name parameters                                                                      \
    {                                                                                        \
        RyContext context;                                                                   \
        ry_context_push(&context);                                                           \
        if (setjmp(context.buffer)) {                                                        \
            ry_context_land(&context);                                                       \
            return 1;                                                                        \
        }                                                                                    \
        *result = call;                                                                      \
        ry_context_pop(&context);                                                            \
        return 0;                                                                            \
    }

RY_ALTREP_TRAMPOLINE(ry_altrep_with_flag,
                     (SEXP (*method)(SEXP, Rboolean), SEXP x, int flag, SEXP *result),
                     method(x, flag ? TRUE : FALSE))
RY_ALTREP_TRAMPOLINE(ry_altrep_coerce, (SEXP (*method)(SEXP, int), SEXP x, int type, SEXP *result),
                     method(x, type))
RY_ALTREP_TRAMPOLINE(ry_altrep_state, (SEXP (*method)(SEXP), SEXP x, SEXP *result), method(x))
RY_ALTREP_TRAMPOLINE(ry_altrep_unserialize,
                     (SEXP (*method)(SEXP, SEXP), SEXP class, SEXP state, SEXP *result),
                     method(class, state))
RY_ALTREP_TRAMPOLINE(ry_altrep_unserialize_ex,
                     (SEXP (*method)(SEXP, SEXP, SEXP, int, int), SEXP class, SEXP state,
                      SEXP attributes, int object, int levels, SEXP *result),
                     method(class, state, attributes, object, levels))
RY_ALTREP_TRAMPOLINE(ry_altrep_extract_subset,
                     (SEXP (*method)(SEXP, SEXP, SEXP), SEXP x, SEXP indices, SEXP call,
                      SEXP *result),
                     method(x, indices, call))
RY_ALTREP_TRAMPOLINE(ry_altrep_inspect,
                     (Rboolean (*method)(SEXP, int, int, int, void (*)(SEXP, int, int, int)),
                      SEXP x, int pre, int deep, int pvec, void (*subtree)(SEXP, int, int, int),
                      int *result),
                     method(x, pre, deep, pvec, subtree))

/* Serialization streams (crates/libr/src/serialize.rs). A stream's callbacks and persistence
 * hooks are native code, so each runs inside its own context. */

int ry_stream_out_bytes(void (*function)(R_outpstream_t, void *, int), R_outpstream_t stream,
                        void *bytes, int length)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        return 1;
    }
    function(stream, bytes, length);
    ry_context_pop(&context);
    return 0;
}

int ry_stream_in_bytes(void (*function)(R_inpstream_t, void *, int), R_inpstream_t stream,
                       void *bytes, int length)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        return 1;
    }
    function(stream, bytes, length);
    ry_context_pop(&context);
    return 0;
}

int ry_stream_in_char(int (*function)(R_inpstream_t), R_inpstream_t stream, int *result)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        return 1;
    }
    *result = function(stream);
    ry_context_pop(&context);
    return 0;
}

int ry_stream_hook(SEXP (*function)(SEXP, SEXP), SEXP object, SEXP data, SEXP *result)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        return 1;
    }
    *result = function(object, data);
    ry_context_pop(&context);
    return 0;
}

/* ------------------------------------------------------------------------------------------
 * Connections (crates/libr/src/connections.rs).
 *
 * The methods of a custom connection are package code, so Rust reaches them only through these
 * trampolines. The default vfprintf lives here because it takes a va_list.
 * ------------------------------------------------------------------------------------------ */

#include <R_ext/Connections.h>

#define RY_CONNECTION_ENTER                                                                     \
    RyContext context;                                                                          \
    ry_context_push(&context);                                                                  \
    if (setjmp(context.buffer)) {                                                               \
        ry_context_land(&context);                                                              \
        return 1;                                                                               \
    }

#define RY_CONNECTION_LEAVE                                                                     \
    ry_context_pop(&context);                                                                   \
    return 0;

/* Formats into R_alloc memory, which the enclosing native scope reclaims even when write
   raises an error, and hands the text to the connection's write method. */
int ry_connection_vfprintf(Rconnection con, const char *format, va_list arguments)
{
    char small[1024];
    va_list copy;
    va_copy(copy, arguments);
    int length = vsnprintf(small, sizeof small, format, arguments);
    char *text = small;
    if (length >= (int) sizeof small) {
        text = R_alloc((size_t) length + 1, 1);
        vsnprintf(text, (size_t) length + 1, format, copy);
    }
    va_end(copy);
    if (length > 0)
        con->write(text, 1, (size_t) length, con);
    return length;
}

int ry_connection_open(Rboolean (*method)(Rconnection), Rconnection con, int *result)
{
    RY_CONNECTION_ENTER
    *result = method(con);
    RY_CONNECTION_LEAVE
}

int ry_connection_void(void (*method)(Rconnection), Rconnection con)
{
    RY_CONNECTION_ENTER
    method(con);
    RY_CONNECTION_LEAVE
}

int ry_connection_int(int (*method)(Rconnection), Rconnection con, int *result)
{
    RY_CONNECTION_ENTER
    *result = method(con);
    RY_CONNECTION_LEAVE
}

int ry_connection_seek(double (*method)(Rconnection, double, int, int), Rconnection con,
                       double position, int origin, int rw, double *result)
{
    RY_CONNECTION_ENTER
    *result = method(con, position, origin, rw);
    RY_CONNECTION_LEAVE
}

int ry_connection_read(size_t (*method)(void *, size_t, size_t, Rconnection), void *buffer,
                       size_t size, size_t count, Rconnection con, size_t *result)
{
    RY_CONNECTION_ENTER
    *result = method(buffer, size, count, con);
    RY_CONNECTION_LEAVE
}

int ry_connection_write(size_t (*method)(const void *, size_t, size_t, Rconnection),
                        const void *buffer, size_t size, size_t count, Rconnection con,
                        size_t *result)
{
    RY_CONNECTION_ENTER
    *result = method(buffer, size, count, con);
    RY_CONNECTION_LEAVE
}

static int ry_connection_printf(int (*method)(Rconnection, const char *, va_list), Rconnection con,
                                const char *format, ...)
{
    va_list arguments;
    va_start(arguments, format);
    int result = method(con, format, arguments);
    va_end(arguments);
    return result;
}

int ry_connection_print(int (*method)(Rconnection, const char *, va_list), Rconnection con,
                        const char *text, int *result)
{
    RY_CONNECTION_ENTER
    *result = ry_connection_printf(method, con, "%s", text);
    RY_CONNECTION_LEAVE
}

/*
 * Numerical routines (applic.rs). The objective functions, gradients and integrands that
 * packages hand to the optimizers and to the quadrature are native code, so each kind runs in
 * a context of its own: an error inside one returns here as status 1 and unwinds through the
 * Rust optimizer as an ordinary Err.
 */

#include <R_ext/Applic.h>

int ry_call_optimfn(optimfn *fn, int n, double *par, void *ex, double *result)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        return 1;
    }
    *result = fn(n, par, ex);
    ry_context_pop(&context);
    return 0;
}

int ry_call_optimgr(optimgr *gr, int n, double *par, double *gradient, void *ex)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        return 1;
    }
    gr(n, par, gradient, ex);
    ry_context_pop(&context);
    return 0;
}

int ry_call_integrand(integr_fn *f, double *x, int n, void *ex)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        return 1;
    }
    f(x, n, ex);
    ry_context_pop(&context);
    return 0;
}

int ry_call_fcn(fcn_p fcn, int n, double *x, double *value, void *state)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        return 1;
    }
    fcn(n, x, value, state);
    ry_context_pop(&context);
    return 0;
}

int ry_call_d2fcn(d2fcn_p d2fcn, int nr, int n, double *x, double *h, void *state)
{
    RyContext context;
    ry_context_push(&context);
    if (setjmp(context.buffer)) {
        ry_context_land(&context);
        return 1;
    }
    d2fcn(nr, n, x, h, state);
    ry_context_pop(&context);
    return 0;
}

/*
 * samin draws its Gaussian steps from norm_rand, which the Rmath module defines. The weak
 * reference lets this library link without that module; samin then reports the gap as an
 * error instead of crashing.
 */
extern double norm_rand(void) __attribute__((weak));

int ry_norm_rand_available(void)
{
    return norm_rand != NULL;
}

double ry_norm_rand(void)
{
    return norm_rand();
}

/* Fortran's error and warning calls. GNU R caps the message at 255 characters. */

static int ry_fortran_length(const char *message, int length)
{
    if (length < 0)
        length = (int) strlen(message);
    return length > 255 ? 255 : length;
}

RY_EXPORT void rexitc_(char *message, int *length)
{
    Rf_error("%.*s", ry_fortran_length(message, *length), message);
}

RY_EXPORT void rwarnc_(char *message, int *length)
{
    Rf_warning("%.*s", ry_fortran_length(message, *length), message);
}

/* The forms without a length take it from Fortran's hidden trailing argument. */
RY_EXPORT void rexit_(char *message, size_t length)
{
    Rf_error("%.*s", ry_fortran_length(message, (int) length), message);
}

RY_EXPORT void rwarn_(char *message, size_t length)
{
    Rf_warning("%.*s", ry_fortran_length(message, (int) length), message);
}
