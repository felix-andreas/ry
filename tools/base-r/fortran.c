/* The two routines from outside BLAS and LAPACK that ry's libRblas and libRlapack call, so that
 * neither needs GNU R's libR or a Fortran runtime (lapack.sh). */
#include <stddef.h>
#include <string.h>

int ry_direct_fortran_active(void);
void Rf_error(const char *format, ...);

/* The reference error handler, as GNU R's libR defines it: an illegal argument raises an R
 * error. When the interpreter calls a routine from Rust, jumping would cross Rust frames, so the
 * handler returns and the routine reports the negative `info` to its caller, which raises the
 * error instead. */
void xerbla_(const char *routine, int *info, size_t routine_length)
{
    if (ry_direct_fortran_active())
        return;
    char name[7];
    size_t length = routine_length < 6 ? routine_length : 6;
    memcpy(name, routine, length);
    name[length] = '\0';
    Rf_error("BLAS/LAPACK routine '%6s' gave error code %d", name, -*info);
}

/* gfortran's `//` on character values (`ilaenv`'s routine names), with libgfortran's contract:
 * the result has `destination_length` characters, truncated or padded with blanks. Defining it
 * here spares the libraries libgfortran, whose static archive is not position independent on
 * every distribution and whose shared library a system without LAPACK often lacks too. */
void _gfortran_concat_string(size_t destination_length, char *destination, size_t left_length,
                             const char *left, size_t right_length, const char *right)
{
    if (left_length >= destination_length) {
        memcpy(destination, left, destination_length);
        return;
    }
    memcpy(destination, left, left_length);
    destination += left_length;
    destination_length -= left_length;
    if (right_length >= destination_length) {
        memcpy(destination, right, destination_length);
        return;
    }
    memcpy(destination, right, right_length);
    memset(destination + right_length, ' ', destination_length - right_length);
}
