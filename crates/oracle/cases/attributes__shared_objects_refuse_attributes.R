# `NULL`, a primitive function and a symbol are each one object for the whole session, so an
# attribute setter refuses them and later uses of the object carry no attributes.
t <- function(e) { r <- withCallingHandlers(tryCatch(e, error = function(e) paste("error:", conditionMessage(e))), warning = function(w) { cat("warning:", conditionMessage(w), "\n"); invokeRestart("muffleWarning") }); cat(deparse(r)[1], "\n") }
t(`comment<-`(NULL, "a")); t(`levels<-`(NULL, "a")); t(`oldClass<-`(NULL, "a")); t(`class<-`(NULL, "a"))
t(`attr<-`(NULL, "a", 1)); t(`names<-`(NULL, "a")); t(`dim<-`(NULL, 1)); t(`dimnames<-`(NULL, list("a"))); t(`tsp<-`(NULL, c(1, 2, 1)))
t(`levels<-`(NULL, NULL)); t(`comment<-`(NULL, NULL)); t(`dimnames<-`(NULL, NULL)); t(`attributes<-`(NULL, list(a = 1))); t(`mostattributes<-`(NULL, list(a = 1)))
z <- list(); t({ levels(z$f)[1] <- "a"; z }); t({ x <- NULL; attr(x, "a") <- 1; x }); t({ y <- NULL; names(y) <- character(0); y })
t(attributes(NULL)); t(NULL); t(class(NULL)); t(levels(NULL)); t(list()$f)
t(`comment<-`(sum, "a")); t(`attr<-`(sum, "a", 1)); t(`levels<-`(sum, "a")); t(`environment<-`(sum, globalenv())); t(`dimnames<-`(sum, list()))
t(attributes(sum)); t(environment(sum)); t(sum(1, 2))
t(`attr<-`(quote(a), "x", 1)); t(`comment<-`(quote(a), "c")); t(attributes(quote(a))); t(quote(a))
f <- function(v) tryCatch({ attr(v, "k") <- 1; v }, error = function(e) conditionMessage(e))
for (i in 1:3) t(f(NULL)); t(f(sum)); t(attributes(NULL))
