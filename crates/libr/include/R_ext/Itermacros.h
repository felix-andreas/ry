/*
 * ry's version of R's region iteration macros: they walk a vector in blocks through the
 * *_GET_REGION accessors, so ALTREP vectors never need to materialize a data pointer.
 */
#ifndef R_EXT_ITERMACROS_H_
#define R_EXT_ITERMACROS_H_

#define LOOP_WITH_INTERRUPT_CHECK(LOOP, ncheck, n, ...)                                      \
    do {                                                                                     \
        for (size_t ry_block_start = 0; ry_block_start < (size_t) (n);                       \
             ry_block_start += (ncheck)) {                                                   \
            size_t ry_block_end = ry_block_start + (ncheck);                                 \
            if (ry_block_end > (size_t) (n))                                                 \
                ry_block_end = (size_t) (n);                                                 \
            LOOP(ry_block_start, ry_block_end, __VA_ARGS__);                                  \
            R_CheckUserInterrupt();                                                          \
        }                                                                                    \
    } while (0)

#define R_ITERATE_CHECK(ncheck, n, i, loop_body)                                             \
    do {                                                                                     \
        for (R_xlen_t i = 0; i < (n); i++) {                                                 \
            loop_body;                                                                       \
            if ((i + 1) % (ncheck) == 0)                                                     \
                R_CheckUserInterrupt();                                                      \
        }                                                                                    \
    } while (0)

#define R_ITERATE(n, i, loop_body) R_ITERATE_CHECK(100000000, n, i, loop_body)

#define ITERATE_BY_REGION_PARTIAL(sx, px, idx, nb, etype, vtype, strt, nfull, expr)        \
    do {                                                                                     \
        const etype *px = (const etype *) DATAPTR_OR_NULL(sx);                               \
        if (px != NULL) {                                                                    \
            R_xlen_t ry_end = (strt) + (nfull);                                              \
            R_xlen_t nb = (nfull);                                                           \
            R_xlen_t idx = (strt);                                                           \
            px = px + (strt);                                                                \
            (void) ry_end;                                                                   \
            do {                                                                             \
                expr                                                                         \
            } while (0);                                                                     \
        } else {                                                                             \
            etype ry_buffer[512];                                                            \
            R_xlen_t ry_end = (strt) + (nfull);                                              \
            for (R_xlen_t idx = (strt); idx < ry_end; idx += 512) {                          \
                R_xlen_t nb = ry_end - idx > 512 ? 512 : ry_end - idx;                       \
                nb = vtype##_GET_REGION(sx, idx, nb, ry_buffer);                             \
                const etype *px = ry_buffer;                                                 \
                do {                                                                         \
                    expr                                                                     \
                } while (0);                                                                 \
            }                                                                                \
        }                                                                                    \
    } while (0)

#define ITERATE_BY_REGION_PARTIAL0(sx, px, idx, nb, etype, vtype, strt, nfull, expr)       \
    ITERATE_BY_REGION_PARTIAL(sx, px, idx, nb, etype, vtype, strt, nfull, expr)

#define ITERATE_BY_REGION(sx, px, idx, nb, etype, vtype, expr)                              \
    ITERATE_BY_REGION_PARTIAL(sx, px, idx, nb, etype, vtype, 0, XLENGTH(sx), expr)

#define ITERATE_BY_REGION0(sx, px, idx, nb, etype, vtype, expr)                             \
    ITERATE_BY_REGION(sx, px, idx, nb, etype, vtype, expr)

#endif
