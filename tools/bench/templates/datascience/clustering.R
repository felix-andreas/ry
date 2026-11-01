#@ weight 2
#@ param CENTERS int 2 5
#@ param ROWS int 30 200
# Unsupervised grouping with k-means and hierarchical clustering.
#: fn([rows]: integer, [centers]: integer, [seed]: integer) -> list{points: matrix, membership: integer[]}
blobs_@ID@ <- function(rows = @ROWS@L, centers = @CENTERS@L, seed = 6L) {
  set.seed(seed)
  means <- matrix(seq_len(centers * 2L) * 6, nrow = centers)
  membership <- sample(seq_len(centers), rows, replace = TRUE)
  #: matrix
  centres <- means[membership, , drop = FALSE]
  points <- centres + matrix(stats::rnorm(rows * 2L), ncol = 2L)
  list(points = points, membership = membership)
}

#: fn(points: matrix, [centers]: integer) -> integer[]
cluster_kmeans_@ID@ <- function(points, centers = @CENTERS@L) {
  stats::kmeans(points, centers = centers, nstart = 5L)$cluster
}

#: fn(points: matrix, [centers]: integer) -> integer[]
cluster_hierarchical_@ID@ <- function(points, centers = @CENTERS@L) {
  stats::cutree(stats::hclust(stats::dist(points), method = "ward.D2"), k = centers)
}

#: fn(cluster: integer[], truth: integer[]) -> double
purity_@ID@ <- function(cluster, truth) {
  #: integer[]
  majority <- apply(table(cluster, truth), 1, max)
  sum(majority) / length(truth)
}
#@ test
test_that("clusters @ID@ match the generating groups", {
  data <- blobs_@ID@()
  expect_gt(purity_@ID@(cluster_kmeans_@ID@(data$points), data$membership), 0.9)
  expect_gt(purity_@ID@(cluster_hierarchical_@ID@(data$points), data$membership), 0.9)
})
