# oracle: requires knitr, rmarkdown
# `R CMD build` builds vignettes with `tools::buildVignettes`, here a knitr vignette that plots.
package <- file.path(tempfile(), "vigplot")
dir.create(file.path(package, "vignettes"), recursive = TRUE)
dir.create(file.path(package, "R"))
writeLines(c(
  "Package: vigplot",
  "Version: 1.0.0",
  "Title: A Plotting Vignette",
  "Description: Builds a vignette that plots.",
  "License: MIT",
  "VignetteBuilder: knitr",
  "Suggests: knitr, rmarkdown"
), file.path(package, "DESCRIPTION"))
writeLines("export(double_it)", file.path(package, "NAMESPACE"))
writeLines("double_it <- function(x) 2 * x", file.path(package, "R", "double.R"))
writeLines(c(
  "---",
  "title: Intro",
  "output: rmarkdown::html_vignette",
  "vignette: >",
  "  %\\VignetteIndexEntry{Intro}",
  "  %\\VignetteEngine{knitr::rmarkdown}",
  "  %\\VignetteEncoding{UTF-8}",
  "---",
  "",
  "```{r}",
  "plot(1:10, (1:10)^2)",
  "```",
  "",
  "```{r}",
  "library(grid)",
  "grid.newpage()",
  "grid.circle(gp = gpar(fill = radialGradient(c('white', 'navy'))))",
  "```"
), file.path(package, "vignettes", "intro.Rmd"))
built <- tools::buildVignettes(dir = package, tangle = TRUE, quiet = TRUE)
print(sort(basename(unlist(built$outputs))))
print(sort(list.files(file.path(package, "vignettes"))))
html <- readLines(file.path(package, "vignettes", "intro.html"))
print(sum(lengths(regmatches(html, gregexpr("data:image/png;base64", html)))))
