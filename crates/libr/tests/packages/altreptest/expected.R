# Records GNU R's ALTREP conformance report, which ry's must reproduce.
#
#   Rscript expected.R <altreptest.so built by R CMD SHLIB> <output file>
#
# crates/libr/tests/test_altrep.rs runs this when GNU R is installed and compares the result
# with the committed expected.txt.
arguments <- commandArgs(trailingOnly = TRUE)
dyn.load(arguments[[1]])
writeLines(.Call("altreptest_report"), arguments[[2]])
