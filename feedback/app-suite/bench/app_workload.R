# app_workload.R: one workload with the time mix of an application test suite.
#
# The suite it models tests a large business application: S4 objects with validity checks, small
# data.table and xts objects, string keys, dates, condition handlers and many small functions.
# No single function dominates such a suite. So this workload has 13 sections, and each section
# runs for a fixed number of rounds. The rounds are set so that the share of each section in GNU
# R's time is the share that this kind of code has in the suite's GNU R time (see `rounds`).
#
# Score the workload by its total time. Do not score it by the geometric mean of the sections:
# a section that becomes 10 times as fast then hides a section that is not faster at all.
#
# Usage: [SCALE=1] [SECTIONS=bodies,objects] Rscript app_workload.R   (or: ry run app_workload.R)
# It prints one line per section, `section  seconds  checksum`, and the total.

suppressPackageStartupMessages({
  library(methods); library(data.table); library(xts); library(fastmap); library(testthat)
})
# The suite runs data.table with one thread, because its tables are small and it runs several
# test processes per machine.
setDTthreads(1)

# GNU R seconds per section at SCALE=1 are close to the numbers in the comments.
rounds <- c(
  bodies = 175000,      # 3.0 s  plain function bodies and argument checks
  objects = 6500,       # 2.4 s  S4 objects: new(), validity, generics, slots, is()
  keys = 20500,         # 1.8 s  paste, sprintf, %in%, match, unique, grepl
  functionals = 31000,  # 1.8 s  lapply, sapply, vapply, Map, Reduce, do.call
  dates = 4000,         # 1.4 s  Date, POSIXct, format, strptime
  conditions = 50000,   # 1.4 s  tryCatch, withCallingHandlers, on.exit, sys.calls
  tables = 610,         # 1.8 s  small data.table objects
  series = 1500,        # 1.6 s  small xts objects
  frames = 1600,        # 0.6 s  data.frame
  closures = 3800,      # 1.0 s  objects made of closures, fastmap, S3 operators
  native = 50,          # 2.3 s  large inputs, where the time is in compiled package code
  expectations = 50,    # 1.2 s  testthat expectations
  storage = 180         # 0.4 s  serialize, saveRDS, readRDS
)

# --- bodies -------------------------------------------------------------------------------------
field <- function(x, name) x[[name]]
weight_of <- function(kind) if (kind == "a") 1 else if (kind == "b") 2 else if (kind == "c") 3 else 4
is_blank <- function(x) {
  if (is.null(x)) return(TRUE)
  ifelse(length(x) > 1, all(is.na(x)), !isTRUE(!is.na(unname(x))))
}
check_types <- function(..., types) {
  values <- list(...)
  for (k in seq_along(values)) {
    if (!identical(class(values[[k]])[1], types[[k]]) && !is(values[[k]], types[[k]])) stop("bad type")
  }
  invisible(TRUE)
}
summarize_record <- function(x, scale = 1, kinds = c("a", "b", "c"), verbose = FALSE) {
  check_types(x, scale, types = c("list", "numeric"))
  if (is_blank(x$id)) return(NULL)
  kind <- field(x, "kind")
  if (!(kind %in% kinds)) stop("unknown kind")
  weight <- weight_of(kind)
  size <- x$size * scale
  bounds <- x$inner
  width <- bounds$high - bounds$low
  total <- 0
  for (tag in x$tags) if (nchar(tag) > 1L) total <- total + weight
  if (verbose) message("total ", total)
  result <- list(id = x$id, size = size, width = width, total = total)
  if (width > 100) result$wide <- TRUE
  result
}
record <- list(id = "k1", kind = "b", size = 3, tags = c("t1", "t2"), inner = list(low = 1, high = 9))
bodies <- function(n) {
  total <- 0
  for (i in seq_len(n)) total <- total + summarize_record(record, scale = i)$size
  total
}

# --- objects ------------------------------------------------------------------------------------
setClass("Base", representation("VIRTUAL", id = "character", day = "Date"))
setClass("Item", contains = "Base", representation(value = "numeric", kind = "character", tags = "list"))
invisible(setValidity("Item", function(object) {
  if (length(object@id) != 1L) "id must have length 1" else if (length(object@kind) > length(object@value)) "kind is longer than value" else TRUE
}))
invisible(setGeneric("value", function(object, ...) standardGeneric("value")))
invisible(setGeneric("value<-", function(object, value) standardGeneric("value<-")))
invisible(setGeneric("label_of", function(object, ...) standardGeneric("label_of")))
setMethod("value", "Item", function(object, scale = 1, ...) object@value * scale)
setReplaceMethod("value", "Item", function(object, value) { object@value <- value; validObject(object); object })
setMethod("label_of", "Base", function(object, ...) paste(object@id, format(object@day)))
setMethod("label_of", "Item", function(object, ...) paste(callNextMethod(), object@kind[1]))
setMethod("initialize", "Item", function(.Object, ...) {
  .Object <- callNextMethod(.Object, ...)
  if (length(.Object@kind) == 0L) .Object@kind <- rep("plain", length(.Object@value))
  .Object
})
day0 <- as.Date("2024-01-02")
objects <- function(n) {
  total <- 0
  for (i in seq_len(n)) {
    item <- new("Item", id = "id1", day = day0, value = c(1, 2, i))
    value(item) <- value(item, scale = 2)
    if (is(item, "Base") && isVirtualClass("Base")) total <- total + sum(item@value)
    slots <- lapply(slotNames(item), function(name) slot(item, name))
    total <- total + length(slots) + nchar(label_of(item))
  }
  total
}

# --- keys ---------------------------------------------------------------------------------------
ids <- paste0("id", 1:400)
kinds <- rep(c("px", "vol", "rate", "fx"), 100)
keys <- function(n) {
  total <- 0
  for (i in seq_len(n)) {
    wanted <- paste(ids[1:20], kinds[1:20], sep = "|")
    have <- paste(ids, kinds, sep = "|")
    total <- total + sum(wanted %in% have) + match("id77|px", have) + length(unique(kinds))
    label <- sprintf("%s_%03d_%.2f", "run", i %% 1000L, i / 7)
    total <- total + nchar(label) + sum(grepl("^id1", ids[1:30])) + length(setdiff(ids[1:30], ids[10:40]))
  }
  total
}

# --- functionals --------------------------------------------------------------------------------
functionals <- function(n) {
  xs <- as.list(1:10)
  total <- 0
  for (i in seq_len(n)) {
    a <- lapply(xs, function(x) x + i)
    b <- sapply(xs, function(x) x * 2)
    d <- vapply(xs, function(x) x > 5, logical(1))
    e <- Map(function(x, y) x + y, xs, a)
    f <- Reduce(`+`, a)
    g <- do.call(c, e)
    h <- unlist(lapply(a, function(x) if (x %% 2 == 0) x))
    total <- total + f + sum(b) + sum(d) + length(g) + length(h)
  }
  total
}

# --- dates --------------------------------------------------------------------------------------
dates <- function(n) {
  days <- day0 + 0:29
  stamps <- as.POSIXct("2024-01-02 09:30:00", tz = "UTC") + 3600 * 0:29
  total <- 0
  for (i in seq_len(n)) {
    text <- format(days)
    back <- as.Date(text)
    parts <- as.POSIXlt(stamps)
    total <- total + as.numeric(back[30] - back[1]) + sum(parts$hour) + parts$year[1]
    total <- total + nchar(format(stamps[1], "%Y-%m-%d %H:%M")) + as.numeric(as.Date(stamps[2]))
    total <- total + sum(weekdays(days[1:5]) == "Monday") + as.numeric(max(days) - min(days))
    total <- total + length(seq(days[1], by = "month", length.out = 6))
  }
  total
}

# --- conditions ---------------------------------------------------------------------------------
risky <- function(i) {
  on.exit(NULL)
  if (i %% 10 == 0) warning("tenth")
  if (i %% 25 == 0) stop("twenty-fifth")
  stopifnot(is.numeric(i), length(i) == 1L)
  i
}
conditions <- function(n) {
  total <- 0
  for (i in seq_len(n)) {
    total <- total + withCallingHandlers(
      tryCatch(risky(i), error = function(e) 0, finally = NULL),
      warning = function(w) invokeRestart("muffleWarning")
    )
    total <- total + length(sys.calls()) + tryCatch(i, error = function(e) 0)
    total <- total + inherits(try(if (i %% 50 == 0) stop("x") else 1, silent = TRUE), "try-error")
  }
  total
}

# --- tables -------------------------------------------------------------------------------------
tables <- function(n) {
  total <- 0
  for (i in seq_len(n)) {
    dt <- data.table(id = ids[1:40], kind = kinds[1:40], value = as.numeric(1:40), day = day0 + 1:40)
    dt[, double := value * 2]
    part <- dt[kind == "px"]
    sums <- dt[, .(total = sum(value)), by = kind]
    setkey(dt, id)
    joined <- merge(dt, sums, by = "kind")
    both <- rbindlist(list(part, part, part))
    ordered <- dt[order(-value)]
    set(dt, j = "flag", value = dt$value > 20)
    total <- total + nrow(part) + sums$total[1] + nrow(joined) + nrow(both) + ordered$value[1] + sum(dt$flag)
    total <- total + dt["id7", value] + length(unique(dt$kind)) + ncol(copy(dt))
  }
  total
}

# --- series -------------------------------------------------------------------------------------
series <- function(n) {
  days <- day0 + 0:59
  x <- xts(cbind(a = as.numeric(1:60), b = as.numeric(60:1)), order.by = days)
  y <- xts(cbind(c = as.numeric(1:30)), order.by = days[seq(1, 60, 2)])
  total <- 0
  for (i in seq_len(n)) {
    part <- x[days[10:20]]
    both <- merge(x, y)
    total <- total + sum(coredata(part)) + nrow(both) + as.numeric(index(x)[1])
    total <- total + as.numeric(last(x$a)) + sum(is.na(both$c)) + nrow(x["2024-02"])
    total <- total + sum(coredata(lag(x$a, 1)), na.rm = TRUE) + nrow(rbind(part, x[1:3]))
    total <- total + nrow(na.omit(both)) + ncol(cbind(x, x$a * 2))
  }
  total
}

# --- frames -------------------------------------------------------------------------------------
frames <- function(n) {
  total <- 0
  for (i in seq_len(n)) {
    df <- data.frame(id = ids[1:20], value = as.numeric(1:20), stringsAsFactors = FALSE)
    df$double <- df$value * 2
    part <- df[df$value > 10, ]
    both <- rbind(part, part)
    total <- total + nrow(both) + sum(df[["double"]]) + ncol(as.data.frame(as.list(df[1, ])))
  }
  total
}

# --- closures -----------------------------------------------------------------------------------
term <- function(name, k) structure(list(name = name, k = k), class = "term")
"*.term" <- function(e1, e2) if (is.numeric(e1)) term(e2$name, e2$k * e1) else term(e1$name, e1$k * e2)
"+.terms" <- function(e1, e2) {
  if (inherits(e2, "term")) e1$map$set(e2$name, e2$k) else for (name in e2$map$keys()) e1$map$set(name, e2$map$get(name))
  e1
}
terms <- function() structure(list(map = fastmap()), class = "terms")
closures <- function(n) {
  total <- 0
  for (i in seq_len(n)) {
    model <- terms()
    for (k in 1:12) model <- model + 2 * term(ids[k], k)
    other <- terms() + term("z", 1)
    model <- model + other
    total <- total + model$map$size() + model$map$get("id3")
  }
  total
}

# --- native -------------------------------------------------------------------------------------
native <- function(n) {
  set.seed(1)
  rows <- 20000L
  big <- data.table(id = paste0("id", seq_len(rows) %% 500L), day = day0 + seq_len(rows) %% 250L, value = runif(rows))
  parts <- lapply(1:150, function(k) data.table(day = day0 + 1:100 + k, id = paste0("id", k, "_", 1:100)))
  p <- runif(20000, 0.001, 0.999)
  m <- matrix(runif(250 * 20), 250, 20)
  total <- 0
  for (i in seq_len(n)) {
    grown <- Reduce(rbind, parts)
    stacked <- rbindlist(parts)
    keyed <- setkey(copy(big), id, day)
    sums <- big[, .(value = sum(value)), by = .(id, day)]
    joined <- merge(big, sums[1:2000], by = c("id", "day"))
    q <- pmin(qt(p, 4.5), 3)
    total <- total + nrow(grown) + nrow(stacked) + nrow(keyed) + nrow(joined) + sum(q) + sum(cov(m))
    total <- total + sum(sort(p)[1:10]) + length(unlist(lapply(1:2000, function(k) p[k]))) + nchar(format(big$day[1:5000])[1])
  }
  total
}

# --- expectations -------------------------------------------------------------------------------
expectations <- function(n) {
  total <- 0
  for (i in seq_len(n)) {
    expect_equal(c(a = 1, b = i), c(a = 1, b = i))
    expect_true(i > 0)
    expect_identical(ids[1:3], c("id1", "id2", "id3"))
    expect_length(kinds, 400L)
    expect_error(stop("boom"), "boom")
    total <- total + 5
  }
  total
}

# --- storage ------------------------------------------------------------------------------------
storage <- function(n) {
  file <- tempfile(fileext = ".rds")
  payload <- list(table = data.table(id = ids, kind = kinds, value = as.numeric(1:400)), days = day0 + 0:999, text = rep(ids, 5))
  total <- 0
  for (i in seq_len(n)) {
    saveRDS(payload, file)
    back <- readRDS(file)
    total <- total + nrow(back$table) + length(unserialize(serialize(payload, NULL))$text)
  }
  unlink(file)
  total
}

# --- runner -------------------------------------------------------------------------------------
# The sections run inside withCallingHandlers() and tryCatch(), as the code of a test does.
scale <- as.numeric(Sys.getenv("SCALE", "1"))
chosen <- Sys.getenv("SECTIONS", "")
chosen <- if (nzchar(chosen)) strsplit(chosen, ",")[[1]] else names(rounds)
total <- 0
for (name in chosen) {
  section <- get(name)
  n <- max(1, round(rounds[[name]] * scale))
  suppressWarnings(section(max(1, n %/% 20)))
  start <- sum(proc.time()[1:2])
  check <- withCallingHandlers(
    tryCatch(section(n), error = function(e) stop(e)),
    warning = function(w) invokeRestart("muffleWarning")
  )
  seconds <- sum(proc.time()[1:2]) - start
  total <- total + seconds
  cat(sprintf("%-14s %8.3f  %.6g\n", name, seconds, check))
}
cat(sprintf("%-14s %8.3f\n", "total", total))
