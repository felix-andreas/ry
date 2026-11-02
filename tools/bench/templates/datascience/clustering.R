#@ weight 2
#@ param CENTERS int 2 5
#@ param ROWS int 30 200
# Unsupervised grouping with k-means and hierarchical clustering.
blobs_@ID@ <- function(rows: integer = @ROWS@L, centers: integer = @CENTERS@L, seed: integer = 6L) -> list{points: matrix, membership: integer[]} {
  set.seed(seed)
  means <- matrix(seq_len(centers * 2L) * 6, nrow = centers)
  membership <- sample(seq_len(centers), rows, replace = TRUE)
  centres: matrix <- means[membership, , drop = FALSE]
  points <- centres + matrix(stats::rnorm(rows * 2L), ncol = 2L)
  list(points = points, membership = membership)
}

cluster_kmeans_@ID@ <- function(points: matrix, centers: integer = @CENTERS@L) -> integer[] {
  stats::kmeans(points, centers = centers, nstart = 25L)$cluster
}

cluster_hierarchical_@ID@ <- function(points: matrix, centers: integer = @CENTERS@L) -> integer[] {
  stats::cutree(stats::hclust(stats::dist(points), method = "ward.D2"), k = centers)
}

purity_@ID@ <- function(cluster: integer[], truth: integer[]) -> double {
  majority: integer[] <- apply(table(cluster, truth), 1, max)
  sum(majority) / length(truth)
}
#@ test
test_that("clusters @ID@ match the generating groups", {
  data <- blobs_@ID@()
  expect_gt(purity_@ID@(cluster_kmeans_@ID@(data$points), data$membership), 0.9)
  expect_gt(purity_@ID@(cluster_hierarchical_@ID@(data$points), data$membership), 0.9)
})
