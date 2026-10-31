#@ weight 2
#@ param FACTOR int 2 9
#@ ns exportClasses(Quantity@ID@)
#@ ns exportMethods(scale_by@ID@)
# An S4 class with a validity check and a generic.
methods::setClass(
  "Quantity@ID@",
  representation(amount = "numeric", unit = "character"),
  validity = function(object) {
    if (length(object@unit) != 1L) "unit must be a single string" else TRUE
  }
)

methods::setGeneric("scale_by@ID@", function(x, factor = @FACTOR@) standardGeneric("scale_by@ID@"))

methods::setMethod("scale_by@ID@", "Quantity@ID@", function(x, factor = @FACTOR@) {
  methods::new("Quantity@ID@", amount = x@amount * factor, unit = x@unit)
})

methods::setMethod("show", "Quantity@ID@", function(object) {
  cat("<Quantity@ID@ ", format(sum(object@amount)), " ", object@unit, ">\n", sep = "")
})
#@ test
test_that("quantity @ID@ scales and validates", {
  q <- methods::new("Quantity@ID@", amount = c(1, 2), unit = "kg")
  scaled <- scale_by@ID@(q)
  expect_equal(scaled@amount, c(1, 2) * @FACTOR@)
  expect_error(methods::new("Quantity@ID@", amount = 1, unit = c("a", "b")))
  expect_output(show(q), "Quantity@ID@ 3 kg")
})
