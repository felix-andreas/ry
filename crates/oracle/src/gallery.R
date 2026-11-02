# Renders gallery figures for the oracle's gallery harness. A gallery file registers each
# figure with `figure(name, code)`; its other top-level code sets up shared data. Arguments:
# the output directory, the devices (comma separated), the seed, the repetitions, the
# selection file (lines of `file<TAB>figure`), then the gallery files. Each render writes one
# line to `renders.tsv` in the output directory: file, figure, device, seconds, status.
arguments <- commandArgs(trailingOnly = TRUE)
output <- arguments[[1]]
devices <- strsplit(arguments[[2]], ",", fixed = TRUE)[[1]]
seed <- as.integer(arguments[[3]])
repeats <- as.integer(arguments[[4]])
selection <- read.delim(arguments[[5]], header = FALSE, col.names = c("file", "figure"),
                        colClasses = "character")
files <- arguments[-(1:5)]
renders <- file.path(output, "renders.tsv")

open_device <- function(device, path) {
  switch(device,
    png = png(path, width = 480, height = 360),
    svg = svg(path, width = 5, height = 3.75),
    pdf = pdf(path, width = 5, height = 3.75),
    agg = ragg::agg_png(path, width = 480, height = 360),
    stop("unknown device ", device))
}
extension <- c(png = "png", svg = "svg", pdf = "pdf", agg = "agg.png")

render <- function(code, scope, device, path) {
  open_device(device, path)
  on.exit(while (dev.cur() > 1) dev.off())
  shown <- withVisible(eval(code, new.env(parent = scope)))
  if (shown$visible && (inherits(shown$value, "trellis") || inherits(shown$value, "ggplot")))
    print(shown$value)
  invisible(dev.off())
}

for (file in files) {
  stem <- sub("\\.R$", "", basename(file))
  wanted <- selection$figure[selection$file == stem]
  if (length(wanted) == 0) next
  scope <- new.env(parent = globalenv())
  figures <- list()
  scope$figure <- function(name, code) figures[[name]] <<- substitute(code)
  set.seed(seed)
  sys.source(file, envir = scope, keep.source = FALSE)
  for (name in wanted) {
    for (device in devices) {
      path <- file.path(output, paste0(stem, "__", name, ".", extension[[device]]))
      status <- "ok"
      best <- Inf
      for (attempt in seq_len(repeats)) {
        set.seed(seed)
        start <- proc.time()[["elapsed"]]
        status <- tryCatch({
          suppressWarnings(suppressMessages(render(figures[[name]], scope, device, path)))
          "ok"
        }, error = function(error) gsub("[\t\n]+", " ", conditionMessage(error)))
        best <- min(best, proc.time()[["elapsed"]] - start)
        if (status != "ok") break
      }
      cat(stem, name, device, format(best, digits = 6), status, sep = "\t", file = renders,
          append = TRUE)
      cat("\n", file = renders, append = TRUE)
    }
  }
}
