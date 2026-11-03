# Native fast paths for the methods package. Each entry is put in front of the body of the GNU R
# function it is named after (`initialize` is the package's default `initialize` method). A fast
# path either does the function's whole work, with the same result and the same effects, or does
# nothing observable and lets the original body run.
list(
    new = quote(if (.Internal(ryS4New(environment()))) return(initialize(value, ...))),
    initialize = quote(if (.Internal(ryS4Initialize(environment()))) return(.Object)),
    validObject = quote(if (.Internal(ryS4Valid(object, environment()))) return(TRUE)),
    is = quote(switch(.Internal(ryS4Is(object, environment())), return(FALSE), return(TRUE))),
    extends = quote(switch(.Internal(ryS4Extends(environment())), return(FALSE), return(TRUE))),
    setGeneric = quote(if (.Internal(ryS4SetGeneric(environment()))) return(name)),
    setMethod = quote(if (.Internal(ryS4SetMethod(environment()))) return(invisible(f)))
)
