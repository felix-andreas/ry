#ifndef R_ARITH_H_
#define R_ARITH_H_

#include <R_ext/libextern.h>

#ifdef __cplusplus
extern "C" {
#endif

LibExtern double R_NaN;
LibExtern double R_PosInf;
LibExtern double R_NegInf;
LibExtern double R_NaReal;
LibExtern int R_NaInt;

#define NA_LOGICAL R_NaInt
#define NA_INTEGER R_NaInt
#define NA_REAL R_NaReal

int R_IsNA(double);
int R_IsNaN(double);
int R_finite(double);

#define ISNA(x) R_IsNA(x)

#ifdef __cplusplus
int R_isnancpp(double);
#define ISNAN(x) R_isnancpp(x)
#else
#define ISNAN(x) (isnan(x) != 0)
#endif

#define R_FINITE(x) R_finite(x)

#ifdef __cplusplus
}
#endif

#endif
