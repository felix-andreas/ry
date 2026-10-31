#ifndef R_R_H
#define R_R_H

#ifndef USING_R
#define USING_R
#endif

#ifdef __cplusplus
#include <cstdlib>
#include <cstdio>
#include <climits>
#include <cmath>
#include <cstddef>
#else
#include <stdlib.h>
#include <stdio.h>
#include <limits.h>
#include <math.h>
#include <stddef.h>
#endif

#include <Rconfig.h>
#include <R_ext/Arith.h>
#include <R_ext/Boolean.h>
#include <R_ext/Complex.h>
#include <R_ext/Constants.h>
#include <R_ext/Error.h>
#include <R_ext/Memory.h>
#include <R_ext/Print.h>
#include <R_ext/Random.h>
#include <R_ext/Utils.h>
#include <R_ext/RS.h>

#ifdef __cplusplus
extern "C" {
#endif

void R_FlushConsole(void);

#ifdef __cplusplus
}
#endif

#endif
