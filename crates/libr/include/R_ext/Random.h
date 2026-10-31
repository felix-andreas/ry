#ifndef R_RANDOM_H
#define R_RANDOM_H

#include <R_ext/Boolean.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    WICHMANN_HILL,
    MARSAGLIA_MULTICARRY,
    SUPER_DUPER,
    MERSENNE_TWISTER,
    KNUTH_TAOCP,
    USER_UNIF,
    KNUTH_TAOCP2,
    LECUYER_CMRG
} RNGtype;

typedef enum {
    BUGGY_KINDERMAN_RAMAGE,
    AHRENS_DIETER,
    BOX_MULLER,
    USER_NORM,
    INVERSION,
    KINDERMAN_RAMAGE
} N01type;

typedef enum { ROUNDING, REJECTION } Sampletype;

void GetRNGstate(void);
void PutRNGstate(void);
double unif_rand(void);
/* GNU R declares this in Rmath.h, which ry does not provide yet. */
double R_unif_index(double);

#ifdef __cplusplus
}
#endif

#endif
