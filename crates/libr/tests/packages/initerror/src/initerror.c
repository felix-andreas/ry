/* A library whose static initializer raises an R error, as a C++ global whose constructor
 * calls the API can (Rcpp's `Function c("c")` when `c` is not found). */
#include <Rinternals.h>

__attribute__((constructor)) static void initerror_construct(void)
{
    Rf_error("initerror: the static initializer failed");
}
