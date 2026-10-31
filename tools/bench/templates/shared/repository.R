#@ weight 1
# Version control and project metadata helpers around a scratch repository.
init_repository_@ID@ <- function(directory) {
  repo <- git2r::init(directory)
  git2r::config(repo, user.name = "Bench", user.email = "bench@example.test")
  writeLines("hello", file.path(directory, "README.md"))
  git2r::add(repo, "README.md")
  git2r::commit(repo, "initial commit")
  repo
}

head_message_@ID@ <- function(repo) {
  git2r::commits(repo, n = 1L)[[1]]$summary
}

lock_summary_@ID@ <- function(project = ".") {
  c(library = renv::paths$library(project = project), loadfast = requireNamespace("loadfast", quietly = TRUE))
}
#@ test
test_that("repository @ID@ records a commit", {
  withr::with_tempdir({
    repo <- init_repository_@ID@(getwd())
    expect_equal(head_message_@ID@(repo), "initial commit")
  })
  expect_true(length(lock_summary_@ID@()) == 2L)
})
