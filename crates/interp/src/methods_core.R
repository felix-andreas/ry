# Native fast paths for the methods package. Each entry is put in front of the body of the GNU R
# function it is named after (`initialize` is the package's default `initialize` method). A fast
# path either does the function's whole work, with the same result and the same effects, or does
# nothing observable and lets the original body run.
list(
    new = quote(if (.Internal(ryS4New(environment()))) return(initialize(value, ...))),
    initialize = quote(if (.Internal(ryS4Initialize(environment()))) return(.Object)),
    # When the slot loop would find nothing wrong but a validity method has to run, the fast
    # path binds what the body binds up to and through its slot loop, and the body's statements
    # after the loop run here, unchanged.
    validObject = quote(switch(.Internal(ryS4Valid(object, environment())), return(TRUE), {
        anyStrings <- function(x) if (isTRUE(x)) character() else x
        extends <- rev(classDef@contains)
        for (i in seq_along(extends)) {
            exti <- extends[[i]]
            superClass <- exti@superClass
            if (!exti@simple && !is(object, superClass))
                next
            superDef <- getClassDef(superClass)
            if (is.null(superDef)) {
                errors <- c(errors, paste0("superclass \"", superClass,
                    "\" not defined in the environment of the object's class"))
                break
            }
            validityMethod <- superDef@validity
            if (is.function(validityMethod)) {
                errors <- c(errors, anyStrings(validityMethod(as(object, superClass))))
                if (length(errors))
                    break
            }
        }
        validityMethod <- classDef@validity
        if (length(errors) == 0L && is.function(validityMethod)) {
            errors <- c(errors, anyStrings(validityMethod(object)))
        }
        return(if (length(errors)) {
            if (test)
                errors
            else {
                msg <- gettextf("invalid class %s object", dQuote(Class))
                if (length(errors) > 1L)
                    stop(paste(paste0(msg, ":"), paste(seq_along(errors), errors, sep = ": "),
                        collapse = "\n"), domain = NA)
                else stop(msg, ": ", errors, domain = NA)
            }
        } else TRUE)
    })),
    is = quote(switch(if (missing(class2)) 0L else .Internal(ryS4Is(object, class2)), return(FALSE), return(TRUE))),
    extends = quote(switch(.Internal(ryS4Extends(environment())), return(FALSE), return(TRUE))),
    setGeneric = quote(if (.Internal(ryS4SetGeneric(environment()))) return(name)),
    setMethod = quote(if (.Internal(ryS4SetMethod(environment()))) return(invisible(f))),
    setClass = quote(if (is.function(value <- .Internal(ryS4SetClass(environment())))) return(invisible(value))),
    .checkGroupSigLength = quote(if (.Internal(ryS4CheckGroupSigLength(environment()))) return(funs)),
    setClassUnion = quote(if (is.function(value <- .Internal(ryS4SetClassUnion(environment())))) return(invisible(value))),
    cacheMetaData = quote(if (.Internal(ryS4CacheMetaData(environment()))) return(invisible(NULL)))
)
