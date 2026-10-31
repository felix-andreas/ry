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
