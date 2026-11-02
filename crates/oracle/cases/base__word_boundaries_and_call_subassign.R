# TRE's word boundaries: `\b` matches at both ends of the subject, `\B` at neither,
# and a follow-on gsub search treats its start as a fresh subject.
print(regexpr("\\b", " "))
print(gregexpr("\\b", "ab cd")[[1]])
print(gsub("\\b", "|", "ab cd"))
print(gsub("\\B", "|", "abc de"))
print(gsub("\\<", "<", "one two"))
print(gsub("\\>", ">", "one two"))

# Assigning into a call: a call value is stored as a list element would be, and the
# rebuilt call keeps the formula's class and environment.
f1 <- y ~ x + z
f2 <- y ~ w
f2[2] <- f1[2]
print(f2)
print(class(f2))
print(identical(environment(f2), environment(f1)))
cl <- quote(g(a, b))
cl[[3]] <- quote(h(c))
print(cl)
