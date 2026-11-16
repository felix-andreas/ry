# A repeated group under POSIX rules: TRE gives the second group the longest span, so the
# first optional group stays empty.
print(regmatches("abb", regexec("(a)?(ab)?b*", "abb"))[[1]])
print(sub("(a)?(ab)?b*", "<\\1|\\2>", "abb"))
