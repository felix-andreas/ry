# Records GNU R's serialization of a set of objects, so ry's reader and writer can be checked
# against it without R installed. `test_serialize.rs` runs it (when GNU R is available) and
# compares its output with the committed `cases.tsv`; to refresh that file, run from this
# directory with a UTF-8 locale:
#
#     LC_ALL=C.UTF-8 Rscript generate.R > cases.tsv
#
# Each line is: name, format, version, the stream GNU R writes (hex), and the stream ry must
# write after reading it (hex). The last field is empty when it is the same stream, and `-` when
# ry reads the stream but writes a different, equally valid layout (a hashed environment, which
# ry writes unhashed). It differs where GNU R writes a representation ry reads into a plainer
# object: ALTREP vectors and bytecode, whose expected stream is GNU R's serialization of the
# plain equivalent. The writer's R version in every header is replaced by a fixed one (4.6.2),
# and the test maps the version ry presents to that one, so the streams compare byte for byte
# whichever GNU R is linked.

ry_version <- 4L * 65536L + 6L * 256L + 2L

with_ry_version <- function(bytes, format) {
    if (format == "ascii" || format == "asciihex") {
        text <- rawToChar(bytes)
        lines <- strsplit(text, "\n", fixed = TRUE)[[1]]
        prefix <- paste0(lines[1], "\n", lines[2], "\n")
        rest <- substring(text, nchar(prefix) + nchar(lines[3]) + 2L)
        return(charToRaw(paste0(prefix, ry_version, "\n", rest)))
    }
    endian <- if (format == "xdr") "big" else .Platform$endian
    bytes[7:10] <- writeBin(ry_version, raw(), endian = endian)
    bytes
}

write_stream <- function(object, format, version) {
    switch(format,
        xdr = serialize(object, NULL, xdr = TRUE, version = version),
        binary = serialize(object, NULL, xdr = FALSE, version = version),
        ascii = serialize(object, NULL, ascii = TRUE, version = version),
        asciihex = serialize(object, NULL, ascii = NA, version = version)
    )
}

hex <- function(bytes) paste(as.character(bytes), collapse = "")

unhashed <- function() new.env(hash = FALSE, parent = globalenv())

latin1 <- "caf\xe9"
Encoding(latin1) <- "latin1"
bytes_string <- "\xff\xfe"
Encoding(bytes_string) <- "bytes"

shared <- unhashed()
assign("value", c(1, 2), envir = shared)
cyclic <- unhashed()
assign("self", cyclic, envir = cyclic)
assign("n", 3L, envir = cyclic)
locked <- unhashed()
assign("a", 1, envir = locked)
assign("b", "two", envir = locked)
lockBinding("a", locked)
lockEnvironment(locked)
active <- unhashed()
makeActiveBinding("now", function() 42, active)
promised <- unhashed()
delayedAssign("later", stop("never forced"), assign.env = promised, eval.env = globalenv())
attributed <- unhashed()
attr(attributed, "name") <- "my environment"
class(attributed) <- "registry"
closure_env <- unhashed()
assign("offset", 10, envir = closure_env)
adder <- function(x, y = 2) x + y + offset
environment(adder) <- closure_env
plain_function <- function(x, ...) {
    if (x > 1) list(...) else x[[1L]]
}
setClass("Point", representation(x = "numeric", y = "numeric"))
hashed <- new.env(hash = TRUE, parent = emptyenv())
assign("alpha", 1L, envir = hashed)

cases <- list(
    null = quote(NULL),
    logical = quote(c(TRUE, FALSE, NA)),
    integer = quote(c(1L, -2L, NA, 2147483647L)),
    double = quote(c(0, -0, 0.1, pi, 1e-300, 5e-324, 1e300, -2.5, NA, NaN, Inf, -Inf, 123456789012)),
    complex = quote(c(1+2i, NA, complex(real = -0.5, imaginary = 1e10))),
    raw = quote(as.raw(c(0, 1, 127, 128, 255))),
    strings = quote(c("a", "", NA, "café", latin1, bytes_string, "tab\there", "q\"'?\\", "line\nbreak", " space")),
    empty = quote(list()),
    named_list = quote(list(a = 1L, b = "x", c = list(d = TRUE))),
    factor = quote(factor(c("lo", "hi", "lo"), levels = c("lo", "hi"))),
    data_frame = quote(data.frame(x = c(1, 2), y = c("a", "b"))),
    matrix = quote(matrix(c(1, 2, 3, 4), 2, dimnames = list(c("r1", "r2"), c("c1", "c2")))),
    symbol = quote(as.symbol("my symbol")),
    call = quote(quote(f(x, y = 2, ...))),
    shared_symbols = quote(list(quote(x), quote(x + x), quote(y))),
    function_call = quote(quote(function(x, y = 1) x + y)),
    expression = quote(expression(a, b + 1, "c")),
    formula = quote(y ~ x + z),
    pairlist = quote(pairlist(a = 1, 2, b = NULL)),
    missing_arg = quote(formals(function(x, y = 3) NULL)),
    closure = quote(plain_function),
    closure_env = quote(adder),
    environment = quote(shared),
    environment_cycle = quote(cyclic),
    environment_locked = quote(locked),
    active_binding = quote(active),
    promise = quote(promised),
    environment_attributes = quote(attributed),
    special_envs = quote(list(globalenv(), baseenv(), emptyenv(), asNamespace("base"))),
    namespace = quote(asNamespace("stats")),
    package_env = quote(as.environment("package:stats")),
    s4 = quote(new("Point", x = 1, y = 2)),
    external_pointer = quote(list(p = new("externalptr"), q = 1)),
    primitives = quote(list(.Call, quote)),
    nested = quote(list(list(list(list(1L)), "deep"), NULL)),
    shared_env_twice = quote(list(shared, shared, adder))
)

# Objects GNU R writes in a representation ry reads into something plainer, each with the plain
# equivalent ry must write back, or NULL when ry's layout is different but equally valid.
plainer <- list(
    altrep_intseq = list(quote(1:10), quote(c(1L, 2L, 3L, 4L, 5L, 6L, 7L, 8L, 9L, 10L))),
    altrep_intseq_down = list(quote(5:-2), quote(c(5L, 4L, 3L, 2L, 1L, 0L, -1L, -2L))),
    altrep_realseq = list(quote(3000000002:3000000000), quote(c(3000000002, 3000000001, 3000000000))),
    altrep_deferred = list(quote(as.character(c(1L, 20L, 300L))), quote(c("1", "20", "300"))),
    altrep_wrapper = list(quote(.Internal(wrap_meta(c(1.5, 2), 1L, 0L))), quote(c(1.5, 2))),
    bytecode = list(quote(compiler::cmpfun(plain_function)), quote(plain_function)),
    hashed_env = list(quote(list(hashed)), NULL)
)

formats <- list(
    c("xdr", 2L), c("xdr", 3L), c("binary", 3L), c("ascii", 2L), c("ascii", 3L), c("asciihex", 3L)
)

emit <- function(name, object, expected, different) {
    for (format in formats) {
        version <- as.integer(format[[2]])
        stream <- hex(with_ry_version(write_stream(object, format[[1]], version), format[[1]]))
        expected_stream <- if (different) "-" else
            hex(with_ry_version(write_stream(expected, format[[1]], version), format[[1]]))
        if (identical(expected_stream, stream)) expected_stream <- ""
        cat(name, format[[1]], version, stream, expected_stream, sep = "\t")
        cat("\n")
    }
}

suppressWarnings({
    for (name in names(cases)) {
        object <- eval(cases[[name]])
        emit(name, object, object, FALSE)
    }
    for (name in names(plainer)) {
        pair <- plainer[[name]]
        different <- is.null(pair[[2]])
        emit(name, eval(pair[[1]]), if (different) NULL else eval(pair[[2]]), different)
    }
})
