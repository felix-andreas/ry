# Every loaded namespace carries an S3 methods table, which `.S3methods()` lists.
tables <- vapply(loadedNamespaces(), function(name) {
  is.environment(getNamespace(name)[[".__S3MethodsTable__."]])
}, NA)
all(tables)
methods <- .S3methods(class = "lm")
c("anova.lm", "summary.lm", "print.lm") %in% methods
