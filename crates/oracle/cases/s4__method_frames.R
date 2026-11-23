# What a method that `standardGeneric` runs sees of its frame, the generic's frame and the caller.
# Each call runs several times, so a call that finds its method in the generic's table is covered
# as well as the first one.
suppressMessages(library(methods))
setClass("Base", representation("VIRTUAL", id = "character"))
setClass("Item", contains = "Base", representation(value = "numeric"))
setClass("SubItem", contains = "Item", representation(factor = "numeric"))
item <- new("Item", id = "a", value = 2)
sub <- new("SubItem", id = "b", value = 3, factor = 10)
thrice <- function(f) for (i in 1:3) f()

cat("-- the stack: the call twice, the generic below the method\n")
setGeneric("stack", function(object, ...) standardGeneric("stack"))
setMethod("stack", "Item", function(object, ...) {
    calls <- sys.calls()
    list(
        calls = vapply(calls, function(call) deparse(call)[1], ""),
        nframe = sys.nframe(),
        parent = sys.parent(),
        generic_is = class(sys.function(sys.nframe() - 1L))[1],
        method_is = class(sys.function())[1],
        call = deparse(sys.call()),
        matched = deparse(match.call())
    )
})
caller <- function(thing) stack(thing, extra = 1)
thrice(function() str(caller(item)))

cat("-- parent.frame() is the generic's caller\n")
setGeneric("whose", function(object) standardGeneric("whose"))
setMethod("whose", "Item", function(object) {
    get("marker", envir = parent.frame())
})
outer <- function() {
    marker <- "the caller's local"
    whose(item)
}
thrice(function() print(outer()))
marker <- "global"
thrice(function() print(whose(item)))

cat("-- dispatch variables\n")
setGeneric("vars", function(object, ...) standardGeneric("vars"))
setMethod("vars", "Item", function(object, ...) {
    list(
        generic = as.character(.Generic),
        package = attr(.Generic, "package"),
        target = as.character(.target),
        defined = as.character(.defined),
        method = class(.Method)[1],
        names = sort(setdiff(grep("^\\.", ls(all.names = TRUE), value = TRUE), ".Methods"))
    )
})
thrice(function() str(vars(item)))
thrice(function() str(vars(sub)))

cat("-- the generic's frame binds the method and its classes\n")
setGeneric("below", function(object) standardGeneric("below"))
setMethod("below", "Item", function(object) {
    generic_frame <- sys.frame(sys.nframe() - 1L)
    list(
        sort(ls(generic_frame, all.names = TRUE)),
        as.character(get(".target", generic_frame)),
        as.character(get(".defined", generic_frame)),
        identical(get(".Method", generic_frame), .Method)
    )
})
thrice(function() str(below(item)))
thrice(function() str(below(sub)))
setGeneric("twice", function(object) {
    first <- standardGeneric("twice")
    object <- sub
    second <- standardGeneric("twice")
    c(first, second, as.character(.target), as.character(.defined))
})
setMethod("twice", "Item", function(object) "Item")
setMethod("twice", "SubItem", function(object) "SubItem")
thrice(function() print(twice(item)))

setGeneric("target", function(object) standardGeneric("target"))
setMethod("target", "Item", function(object) as.character(.target))
thrice(function() print(target(sub)))
setGeneric("defined", function(object) standardGeneric("defined"))
setMethod("defined", "Base", function(object) as.character(.defined))
thrice(function() print(defined(sub)))

cat("-- missing arguments and defaults\n")
setGeneric("miss", function(object, extent = "generic default", ...) standardGeneric("miss"))
setMethod("miss", "Item", function(object, extent = paste("method default for", object@id), ...) {
    list(missing(extent), extent, nargs(), names(list(...)))
})
thrice(function() str(miss(item)))
thrice(function() str(miss(item, "given")))
thrice(function() str(miss(item, , tag = 1)))
passes_missing <- function(object, extent) miss(object, extent)
thrice(function() str(passes_missing(item)))

cat("-- a default only the method has is not used\n")
setGeneric("bare", function(object, extent) standardGeneric("bare"))
setMethod("bare", "numeric", function(object, extent = 5) if (missing(extent)) c(1, extent) else extent)
for (i in 1:3) print(tryCatch(bare(1), error = conditionMessage))
for (i in 1:3) print(bare(1, 2))

cat("-- a default the generic read before dispatching stays\n")
setGeneric("early", function(object, extent = 1, scale = stop("never read")) {
    force(extent)
    standardGeneric("early")
})
setMethod("early", "numeric", function(object, extent = 5, scale = nchar(format(object))) {
    list(extent, missing(extent), scale, missing(scale))
})
thrice(function() str(early(123)))
thrice(function() str(early(123, 2, 3)))

cat("-- a default the generic reads after dispatching is the method's\n")
setGeneric("late", function(object, extent = paste("generic"), scale = "generic") {
    result <- standardGeneric("late")
    c(result, extent, scale)
})
setMethod("late", "numeric", function(object, extent = paste("method"), scale = "method") extent)
setMethod("late", "character", function(object, extent = paste("method"), scale = "method") "unread")
thrice(function() print(late(1)))
thrice(function() print(late("a")))
thrice(function() print(late(1, "given")))

cat("-- the method sees what the generic assigned before dispatching\n")
setGeneric("assigned", function(object, extent = 1) {
    object <- "changed"
    standardGeneric("assigned")
})
setMethod("assigned", "numeric", function(object, extent = 5) list(object, extent))
setMethod("assigned", "character", function(object, extent = 6) list(object, extent))
thrice(function() str(assigned(1)))
setGeneric("given", function(object, extent = 1) {
    extent <- 7
    standardGeneric("given")
})
setMethod("given", "numeric", function(object, extent = 5) list(object, extent, missing(extent)))
thrice(function() str(given(1)))
thrice(function() str(given(1, 2)))

cat("-- an invisible result stays invisible\n")
setGeneric("quiet", function(object) standardGeneric("quiet"))
setGeneric("label2", function(object) standardGeneric("label2"))
setMethod("label2", "Item", function(object) object@id)
setMethod("quiet", "Item", function(object) invisible(object@id))
for (i in 1:3) quiet(item)
thrice(function() print(withVisible(quiet(item))$visible))
thrice(function() print(withVisible(label2(item))$visible))

cat("-- a signature argument that is missing\n")
setGeneric("pairs2", function(left, right) standardGeneric("pairs2"))
setMethod("pairs2", signature("Item", "missing"), function(left, right) "missing")
setMethod("pairs2", signature("Item", "Item"), function(left, right) "both")
setMethod("pairs2", signature("Item", "numeric"), function(left, right) "numeric")
thrice(function() print(c(pairs2(item), pairs2(item, sub), pairs2(item, 1), pairs2(right = 2L, left = sub))))

cat("-- arguments are forced once, in signature order, and stay promises\n")
setGeneric("lazy", function(first, second, third) standardGeneric("lazy"))
setMethod("lazy", signature("numeric", "numeric"), function(first, second, third) {
    cat("in the method\n")
    if (first > 1) third else "third never forced"
})
noisy <- function(label, value) {
    cat("forcing", label, "\n")
    value
}
for (i in 1:3) print(lazy(noisy("first", i), noisy("second", 2), noisy("third", "third")))
print(lazy(substitute_me <- 5, 2, 3))
setGeneric("quoted", function(object, expression) standardGeneric("quoted"), signature = "object")
setMethod("quoted", "Item", function(object, expression) deparse(substitute(expression)))
thrice(function() print(quoted(item, a + b * 2)))

cat("-- an error in a signature argument\n")
for (i in 1:3) print(tryCatch(lazy(stop("no value"), 2), error = conditionMessage))
for (i in 1:3) print(tryCatch(lazy(1, undefined_variable), error = conditionMessage))
handled <- withCallingHandlers(
    tryCatch(lazy(stop("seen once"), 2), error = function(e) "caught"),
    error = function(e) cat("calling handler:", conditionMessage(e), "\n")
)
print(handled)

cat("-- `...` reaches the method\n")
setGeneric("dots", function(object, ...) standardGeneric("dots"))
setMethod("dots", "Item", function(object, ...) {
    list(...length(), names(list(...)), deparse(substitute(list(...))))
})
wrapper <- function(...) dots(item, ..., last = 3)
thrice(function() str(wrapper(1, b = 2)))

cat("-- a method with more arguments than its generic\n")
setGeneric("scaled", function(object, ...) standardGeneric("scaled"))
setMethod("scaled", "Item", function(object, by = 2, ...) object@value * by)
setMethod("scaled", "SubItem", function(object, by = 2, ...) callNextMethod() * object@factor)
thrice(function() print(c(scaled(item), scaled(item, by = 3), scaled(sub), scaled(sub, by = 3))))

cat("-- return(), on.exit and a closure that outlives the method's frame\n")
setGeneric("counter", function(object, start) standardGeneric("counter"))
setMethod("counter", "Item", function(object, start) {
    on.exit(cat("leaving the method\n"))
    count <- start
    if (start < 0) return("negative")
    function() {
        count <<- count + object@value
        count
    }
})
for (i in 1:3) {
    step <- counter(item, i)
    print(c(step(), step()))
}
print(counter(item, -1))

cat("-- the method's environment, asked for from the method and from a callee\n")
setGeneric("frames", function(object, local) standardGeneric("frames"))
setMethod("frames", "Item", function(object, local) {
    inner <- function() sort(ls(parent.frame()))
    here <- environment()
    list(
        inner(),
        identical(here, sys.frame(sys.nframe())),
        sort(ls(sys.frame(sys.nframe() - 1L))),
        identical(parent.frame(), parent.frame(2)),
        environmentName(parent.env(here))
    )
})
thrice(function() str(frames(item, 1)))

cat("-- a generic called from a method, and recursion\n")
setGeneric("depth", function(object, n) standardGeneric("depth"))
setMethod("depth", "Item", function(object, n) if (n == 0) sys.nframe() else depth(object, n - 1))
thrice(function() print(depth(item, 3)))

cat("-- a replacement generic changes the caller's object\n")
setGeneric("value<-", function(object, value) standardGeneric("value<-"))
setReplaceMethod("value", "Item", function(object, value) {
    object@value <- value
    object
})
changed <- item
for (i in 1:3) {
    value(changed) <- i
    print(c(changed@value, item@value))
}

cat("-- a method removed between calls\n")
setGeneric("kept", function(object) standardGeneric("kept"))
setMethod("kept", "Item", function(object) "Item")
setMethod("kept", "SubItem", function(object) "SubItem")
calls_kept <- function() tryCatch(kept(sub), error = function(e) "no method")
thrice(function() print(calls_kept()))
invisible(removeMethod("kept", "SubItem"))
thrice(function() print(calls_kept()))
invisible(removeMethod("kept", "Item"))
thrice(function() print(calls_kept()))
setMethod("kept", "Base", function(object) "Base")
thrice(function() print(calls_kept()))

cat("-- lapply and do.call over a generic\n")
setGeneric("label", function(object) standardGeneric("label"))
setMethod("label", "Base", function(object) paste(class(object), object@id))
print(vapply(list(item, sub, item, sub), label, ""))
print(do.call(label, list(sub)))
print(do.call("label", list(object = item)))

cat("-- the generic's frame under a debugging reader\n")
setGeneric("watched", function(object) standardGeneric("watched"))
setMethod("watched", "Item", function(object) {
    generic_frame <- sys.frame(sys.nframe() - 1L)
    c(exists("object", envir = generic_frame, inherits = FALSE), is(get("object", generic_frame), "Item"))
})
thrice(function() print(watched(item)))

cat("-- traceback of an error in a method\n")
setGeneric("fails", function(object) standardGeneric("fails"))
setMethod("fails", "Item", function(object) stop("from the method"))
for (i in 1:3) {
    result <- tryCatch(fails(item), error = function(e) deparse(conditionCall(e)))
    print(result)
}
calls_seen <- NULL
for (i in 1:3) {
    try(withCallingHandlers(fails(item), error = function(e) {
        calls_seen <<- vapply(sys.calls(), function(call) deparse(call)[1], "")
    }), silent = TRUE)
    print(calls_seen[grepl("fails", calls_seen)])
}
