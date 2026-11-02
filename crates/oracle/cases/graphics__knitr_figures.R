# A vignette with base and grid plots, knitted to Markdown and rendered to HTML. The figures
# are compared by name and pixel size, since GNU R's PNG device draws through cairo.
directory <- tempfile()
dir.create(directory)
writeLines(c(
  "---",
  "title: Plots",
  "output: html_document",
  "---",
  "",
  "```{r}",
  "x <- c(1, 4, 2, 8, 5)",
  "summary(x)",
  "```",
  "",
  "```{r scatter, fig.width = 4, fig.height = 3}",
  "plot(x, type = 'b', main = 'base')",
  "```",
  "",
  "```{r grid-plot, fig.width = 3, fig.height = 3}",
  "library(grid)",
  "grid.newpage()",
  "grid.rect(gp = gpar(fill = linearGradient(c('red', 'blue'))))",
  "grid.circle(r = 0.3, gp = gpar(fill = 'white'))",
  "```",
  "",
  "```{r two-plots, fig.width = 2, fig.height = 2}",
  "hist(x)",
  "barplot(x)",
  "```"
), file.path(directory, "plots.Rmd"))
owd <- setwd(directory)
invisible(knitr::knit("plots.Rmd", quiet = TRUE))
markdown <- readLines("plots.md")
print(grep("^!\\[|^## ", markdown, value = TRUE))
png_size <- function(file) {
  header <- readBin(file, "raw", 24)
  stopifnot(identical(header[2:4], charToRaw("PNG")))
  c(width = readBin(header[17:20], "integer", endian = "big"),
    height = readBin(header[21:24], "integer", endian = "big"))
}
for (file in sort(list.files("figure", full.names = TRUE))) {
  cat(file, png_size(file), "\n")
}
html <- rmarkdown::render("plots.Rmd", output_dir = directory, quiet = TRUE)
cat(basename(html), lengths(regmatches(readLines(html), gregexpr("data:image/png;base64", readLines(html)))) |> sum(), "\n")
setwd(owd)
unlink(directory, recursive = TRUE)
