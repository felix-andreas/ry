#ifndef R_RCONFIG_H
#define R_RCONFIG_H

#define HAVE_F77_UNDERSCORE 1
#define IEEE_754 1
#define SUPPORT_UTF8 1
#define SUPPORT_MBCS 1
#define ENABLE_NLS 1
/* Fortran CHARACTER arguments carry a hidden length (FCLEN and FCONE in R_ext/BLAS.h), as in
   GNU R 4.3 and later; a package can define DONT_USE_FC_LEN_T to drop it. */
#ifndef DONT_USE_FC_LEN_T
#define FC_LEN_T size_t
#endif
/* R_ext/Lapack.h offers the corrected dspgv and dtptrs declarations under usePR18534fix. */
#define PR18534fixed 1
#define HAVE_VISIBILITY_ATTRIBUTE 1
#define SIZEOF_SIZE_T 8
#define HAVE_ALLOCA_H 1
#define HAVE_UINTPTR_T 1

#endif
