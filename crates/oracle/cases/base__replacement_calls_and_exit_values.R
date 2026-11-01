# `rep` takes an empty argument as absent.
rep(1:2, , 5)

# A shorter `dimnames` list is padded with NULL, and an empty one removes the dimnames.
m <- matrix(1:4, 2, dimnames = list(c("a", "b")))
dimnames(m)
dimnames(m) <- list()
dimnames(m)
try(matrix(1:4, 2, dimnames = list(1:2, 1:2, 1:2)))

# A replacement closure sees `*tmp*` and the value in its call.
`foo<-` <- function(x, value) {
  print(sys.call())
  x
}
x <- 1
foo(x) <- 2

# `returnValue()` reads the value of the function whose exit code runs, from any depth.
shown <- function() print(returnValue("default"))
returns <- function() {
  on.exit(shown())
  42
}
returns()
explicit <- function() {
  on.exit(shown())
  return(7)
}
explicit()
fails <- function() {
  on.exit(shown())
  stop("boom")
}
try(fails(), silent = TRUE)
shown()

# A POSIX pattern with minimal repetition takes the shortest match at the leftmost start.
spans <- function(pattern, text) {
  found <- gregexpr(pattern, text)[[1]]
  rbind(start = as.vector(found), length = attr(found, "match.length"))
}
spans("a.*?b.*", "axbxx")
spans("a.*b.*?", "axbxb")
spans("q|a.*?c|a.*?b", "axbc axbc")
spans("<.*?>", "<a><b>")
regmatches("k=v; j=w;", regexec("(.*?)=(.*?);", "k=v; j=w;"))
sub("a(.*?)b", "[\\1]", "axxbyyb")

# Correlations: a constant column has NA off the diagonal and 1 on it, except pairwise.
const <- cbind(a = c(1, 1, 1), b = c(1, 2, 3))
suppressWarnings(cor(const))
suppressWarnings(cor(const, use = "pairwise.complete.obs"))
cor(cbind(a = c(1, NA, 3), b = c(1, 2, 3)))
cor(matrix(numeric(0), 0, 2))
for (use in c("everything", "all.obs", "complete.obs", "na.or.complete", "pairwise.complete.obs")) {
  print(try(var(matrix(numeric(0), 3, 0), use = use), silent = TRUE))
}

# `Sys.info()` names its fields as GNU R does.
names(Sys.info())
