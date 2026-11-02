# Symbol names are limited to 10000 bytes, including do.call's argument tags.
args <- list(1)
names(args) <- strrep("a", 10001)
print(tryCatch(do.call("list", args), error = conditionMessage))
print(tryCatch(path.expand(1), error = conditionMessage))

# rep() refuses objects that are not vectors.
print(tryCatch(rep(function() 1, 2), error = conditionMessage))
print(tryCatch(rep_len(function() 1, 2), error = conditionMessage))
print(tryCatch(rep.int(new.env(), 2), error = conditionMessage))
print(rep(list(1, "a"), 2))
