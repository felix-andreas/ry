#@ weight 2
#@ param FACTOR int 2 9
#@ ns exportClasses(Quantity@ID@)
#@ ns exportMethods(scale_by@ID@)
# An S4 class with a validity check and a generic.
type Quantity@ID@ = list{amount: double[], unit: character}

methods::setClass( # ry: allow(object-system)
  "Quantity@ID@",
  representation(amount = "numeric", unit = "character"),
  validity = function(object) {
    unit: character[] <- object@unit
    if (length(unit) != 1L) "unit must be a single string" else TRUE
  }
)

methods::setGeneric("scale_by@ID@", function(x, factor = @FACTOR@) standardGeneric("scale_by@ID@")) # ry: allow(object-system)

methods::setMethod("scale_by@ID@", "Quantity@ID@", function(x, factor = @FACTOR@) { # ry: allow(object-system)
  amount: double[] <- x@amount
  unit: character <- x@unit
  @Quantity@ID@(methods::new("Quantity@ID@", amount = amount * factor, unit = unit))
})

methods::setMethod("show", "Quantity@ID@", function(object) { # ry: allow(object-system)
  amount: double[] <- object@amount
  unit: character <- object@unit
  cat("<Quantity@ID@ ", format(sum(amount)), " ", unit, ">\n", sep = "")
})
#@ test
test_that("quantity @ID@ scales and validates", {
  q <- @Quantity@ID@(methods::new("Quantity@ID@", amount = c(1, 2), unit = "kg"))
  scaled: Quantity@ID@ <- scale_by@ID@(q)
  amount: double[] <- scaled@amount
  expect_equal(amount, c(1, 2) * @FACTOR@)
  expect_error({
    @Quantity@ID@(methods::new("Quantity@ID@", amount = 1, unit = c("a", "b")))
  })
  expect_output(show(q), "Quantity@ID@ 3 kg")
})
