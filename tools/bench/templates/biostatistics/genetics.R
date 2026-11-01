#@ weight 3
#@ param INDIVIDUALS int 30 200
#@ param VARIANTS int 4 30
#@ param FREQUENCY float 0.1 0.5 2
# Genotype matrices and Hardy-Weinberg checks.
#: fn([individuals]: integer, [variants]: integer, [frequency]: double, [seed]: integer) -> matrix
simulate_genotypes_@ID@ <- function(individuals = @INDIVIDUALS@L, variants = @VARIANTS@L, frequency = @FREQUENCY@, seed = 14L) {
  set.seed(seed)
  matrix(
    stats::rbinom(individuals * variants, 2L, frequency),
    nrow = individuals,
    dimnames = list(sprintf("S%03d", seq_len(individuals)), sprintf("rs%04d", seq_len(variants)))
  )
}

#: fn(genotypes: matrix) -> double[]
allele_frequency_@ID@ <- function(genotypes) colMeans(genotypes) / 2

#: fn(genotypes: matrix) -> double[]
hardy_weinberg_@ID@ <- function(genotypes) {
  apply(genotypes, 2, function(g) {
    counts <- tabulate(g + 1L, nbins = 3L)
    n <- sum(counts)
    p <- (2 * counts[[3]] + counts[[2]]) / (2 * n)
    expected <- n * c((1 - p)^2, 2 * p * (1 - p), p^2)
    keep <- expected > 0
    stats::pchisq(sum((counts[keep] - expected[keep])^2 / expected[keep]), df = 1, lower.tail = FALSE)
  })
}

#: fn(...: matrix) -> matrix
stack_cohorts_@ID@ <- function(...) abind::abind(..., along = 1)

#: fn(genotype: integer[], phenotype: double[]) -> double[]
association_@ID@ <- function(genotype, phenotype) {
  #: @if-unknown formula
  model <- phenotype ~ genotype
  fit <- stats::lm(model)
  stats::coef(summary(fit))["genotype", c("Estimate", "Pr(>|t|)")]
}
#@ test
test_that("genotypes @ID@ are consistent", {
  g <- simulate_genotypes_@ID@()
  expect_equal(dim(g), c(@INDIVIDUALS@, @VARIANTS@))
  frequency <- allele_frequency_@ID@(g)
  expect_true(all(frequency >= 0 & frequency <= 1))
  expect_true(all(hardy_weinberg_@ID@(g) >= 0, na.rm = TRUE))
  expect_equal(nrow(stack_cohorts_@ID@(g, g)), 2 * @INDIVIDUALS@)
  set.seed(1)
  #: @if-unknown integer[]
  first <- g[, 1]
  phenotype <- first * 0.5 + rnorm(@INDIVIDUALS@)
  expect_length(association_@ID@(first, phenotype), 2L)
})
