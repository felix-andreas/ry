#ifndef R_RANDOM_H
#define R_RANDOM_H

#include <R_ext/Boolean.h>

#ifdef __cplusplus
extern "C" {
#endif

/* The uniform generators RNGkind() names, in the order of their codes. */
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

/* How normal deviates are made from uniform ones. */
typedef enum {
    BUGGY_KINDERMAN_RAMAGE,
    AHRENS_DIETER,
    BOX_MULLER,
    USER_NORM,
    INVERSION,
    KINDERMAN_RAMAGE
} N01type;

/* How sample() draws a discrete uniform index. */
typedef enum { ROUNDING, REJECTION } Sampletype;
Sampletype R_sample_kind(void);

/* Which variant of the BTPE binomial generator rbinom() uses. */
typedef enum { BUGGY_BTPE, BTPE } Binomtype;
Binomtype R_binom_kind(void);

/* Bracket a run of draws: GetRNGstate() reads .Random.seed, PutRNGstate() writes it back. */
void GetRNGstate(void);
void PutRNGstate(void);

double unif_rand(void);
double R_unif_index(double);
/* Rmath.h declares these too. */
double norm_rand(void);
double exp_rand(void);

/* A package that supplies its own generator (RNGkind("user-supplied")) defines these; R
   looks them up in the package's shared library. */
typedef unsigned int Int32;
double *user_unif_rand(void);
void user_unif_init(Int32);
int *user_unif_nseed(void);
int *user_unif_seedloc(void);
double *user_norm_rand(void);

#ifdef __cplusplus
}
#endif

#endif
