#@ weight 2
#@ param FACTOR int 2 9
#@ untyped
#@ ns exportClasses(Quantity@ID@)
#@ ns exportMethods(scale_by@ID@)
#@ end
# A quantity with a unit: a nominal record in typed code, an S4 class with a validity check and a
# generic in plain R.
#@ typed
type Quantity@ID@ = Record{amount: double[], unit: character}

quantity_@ID@ <- function(amount: double[], unit: character) -> Quantity@ID@ {
  if (length(unit) != 1L) stop("unit must be a single string")
  @Quantity@ID@((amount = amount, unit = unit))
}

scale_by@ID@ <- function(x: Quantity@ID@, factor: double = @FACTOR@) -> Quantity@ID@ {
  quantity_@ID@(x$amount * factor, x$unit)
}

show_quantity_@ID@ <- function(object: Quantity@ID@) -> NULL {
  cat("<Quantity@ID@ ", format(sum(object$amount)), " ", object$unit, ">\n", sep = "")
}
#@ untyped
type Quantity@ID@ = list{amount: double[], unit: character}

methods::setClass( # ry: allow(object-system)
  "Quantity@ID@",
  representation(amount = "numeric", unit = "character"),
  validity = function(object) {
    unit: character[] <- object@unit
    if (length(unit) != 1L) "unit must be a single string" else TRUE
  }
)

quantity_@ID@ <- function(amount: double[], unit: character) -> Quantity@ID@ {
  @Quantity@ID@(methods::new("Quantity@ID@", amount = amount, unit = unit))
}

methods::setGeneric("scale_by@ID@", function(x, factor = @FACTOR@) standardGeneric("scale_by@ID@")) # ry: allow(object-system)

methods::setMethod("scale_by@ID@", "Quantity@ID@", function(x, factor = @FACTOR@) { # ry: allow(object-system)
  amount: double[] <- x@amount
  unit: character <- x@unit
  quantity_@ID@(amount * factor, unit)
})

methods::setMethod("show", "Quantity@ID@", function(object) { # ry: allow(object-system)
  amount: double[] <- object@amount
  unit: character <- object@unit
  cat("<Quantity@ID@ ", format(sum(amount)), " ", unit, ">\n", sep = "")
})

show_quantity_@ID@ <- function(object: Quantity@ID@) -> NULL {
  methods::show(object)
}
#@ end
#@ test
test_that("quantity @ID@ scales and validates", {
  q <- quantity_@ID@(c(1, 2), "kg")
  scaled: Quantity@ID@ <- scale_by@ID@(q)
#@ typed
  expect_equal(scaled$amount, c(1, 2) * @FACTOR@)
#@ untyped
  amount: double[] <- scaled@amount
  expect_equal(amount, c(1, 2) * @FACTOR@)
#@ end
#@ typed
  # The parameter type refuses a vector of units when the code runs.
  units: Any <- c("a", "b")
  expect_error({
    quantity_@ID@(1, units)
    NULL
  })
#@ untyped
  expect_error(quantity_@ID@(1, c("a", "b")))
#@ end
  expect_output(show_quantity_@ID@(q), "Quantity@ID@ 3 kg")
})
