/* A routine named the way a Fortran compiler names `subroutine cube(n, x)`: lower case with a
 * trailing underscore. R finds it as "cube", which is how `.Fortran` and `getNativeSymbolInfo`
 * reach routines in libraries built from Fortran without registration. */
void cube_(int *n, double *x)
{
    for (int i = 0; i < *n; i++)
        x[i] = x[i] * x[i] * x[i];
}
