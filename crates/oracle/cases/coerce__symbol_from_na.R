# A missing value names the symbol `NA`; an empty vector names nothing.
print(as.name(NA_character_))
print(as.name(NA))
print(lapply(c("a", NA), as.name))
print(tryCatch(as.name(character()), error = conditionMessage))
