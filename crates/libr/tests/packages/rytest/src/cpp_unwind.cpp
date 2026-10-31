// The pattern cpp11 and Rcpp use so that an R error unwinding through C++ frames runs their
// destructors: the body runs under R_UnwindProtect, the cleanup throws a C++ exception when R
// jumps, the C++ caller catches it after the stack has unwound, and R_ContinueUnwind resumes
// the R error from there.

#include <Rinternals.h>

namespace {

int destructors = 0;

struct Guard {
    ~Guard() { destructors++; }
};

struct Unwind {
    SEXP token;
};

// An R error longjmps over this frame, so it must not own anything with a destructor.
SEXP body(void *data) {
    return Rf_eval(static_cast<SEXP>(data), R_GlobalEnv);
}

void cleanup(void *data, Rboolean jump) {
    if (jump) {
        throw Unwind{static_cast<SEXP>(data)};
    }
}

}  // namespace

extern "C" SEXP rytest_cpp_unwind(SEXP call) {
    SEXP token = PROTECT(R_MakeUnwindCont());
    try {
        Guard outer;
        SEXP result = R_UnwindProtect(body, call, cleanup, token, token);
        UNPROTECT(1);
        return result;
    } catch (Unwind &unwind) {
        UNPROTECT(1);
        R_ContinueUnwind(unwind.token);
    }
    return R_NilValue;
}

extern "C" SEXP rytest_destructor_count(void) {
    return Rf_ScalarInteger(destructors);
}
