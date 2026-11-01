#@ weight 4
#@ param ROWS int 50 400
#@ param GROUPS int 3 12
#@ param THRESHOLD float 0.2 0.8 2
# Grouped aggregation, joins and reshaping on data.table.
#: @alias Events@ID@ {data.table{id: integer[], group: character[], amount: double[], flag: logical[]}}
#: @alias FeaturedEvents@ID@ {data.table{id: integer[], group: character[], amount: double[], flag: logical[], rank_in_group: integer[], size_class: character[], z: double[]}}

#: fn([rows]: integer, [groups]: integer, [seed]: integer) -> Events@ID@
make_events_@ID@ <- function(rows = @ROWS@L, groups = @GROUPS@L, seed = 1L) {
  set.seed(seed)
  data.table::data.table(
    id = seq_len(rows),
    group = sample(sprintf("g%02d", seq_len(groups)), rows, replace = TRUE),
    amount = round(stats::runif(rows, 0, 100), 2),
    flag = stats::runif(rows) > @THRESHOLD@
  )
}

#: fn(events: Events@ID@) -> data.table{group: character[], n: integer[], total: double[], share_flagged: double[]}
summarise_groups_@ID@ <- function(events) {
  events[, .(n = .N, total = sum(amount), share_flagged = mean(flag)), by = group][order(-total)]
}

#: fn(events: Events@ID@) -> FeaturedEvents@ID@
add_features_@ID@ <- function(events) {
  out <- data.table::copy(events)
  out[, rank_in_group := data.table::frank(-amount, ties.method = "first"), by = group]
  out[, size_class := data.table::fcase(amount < 25, "small", amount < 75, "medium", default = "large")]
  out[, z := (amount - mean(amount)) / stats::sd(amount), by = group]
  out[]
}

#: fn(events: Events@ID@, labels: data.table{group: character[], label: character[]}) -> data.table
join_labels_@ID@ <- function(events, labels) {
  merge(events, labels, by = "group", all.x = TRUE)
}

#: fn(events: FeaturedEvents@ID@) -> data.table
reshape_wide_@ID@ <- function(events) {
  shape <- group ~ size_class
  data.table::dcast(events, shape, value.var = "amount", fun.aggregate = sum)
}
#@ test
test_that("event tables @ID@ aggregate consistently", {
  events <- make_events_@ID@()
  summary <- summarise_groups_@ID@(events)
  expect_equal(sum(summary$n), @ROWS@)
  expect_equal(sum(summary$total), sum(events$amount))
  featured <- add_features_@ID@(events)
  expect_true(all(featured$rank_in_group >= 1L))
  expect_setequal(unique(featured$size_class), intersect(c("small", "medium", "large"), featured$size_class))
  labels <- data.table::data.table(group = unique(events$group), label = toupper(unique(events$group)))
  expect_equal(nrow(join_labels_@ID@(events, labels)), @ROWS@)
  wide <- reshape_wide_@ID@(featured)
  expect_equal(nrow(wide), length(unique(events$group)))
})
