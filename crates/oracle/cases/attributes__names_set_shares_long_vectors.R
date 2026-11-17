# A direct call of an attribute replacement such as `names<-` leaves the caller's vector as it
# was, for long and short vectors and lists alike, and the result reads and writes as a plain
# vector.
long <- as.list(seq_len(100) / 4); short <- as.list(1:3); dbl <- seq_len(100) / 4
nm <- sprintf("k%03d", seq_len(100))
named <- `names<-`(long, nm)
print(is.null(names(long))); print(names(named)[c(1, 100)]); print(named$k050)
named[[2]] <- "changed"; print(long[[2]]); print(named[[2]])
named$k003 <- NULL; print(length(named)); print(length(long))
print(identical(unname(`names<-`(long, nm)), long))
s <- `names<-`(short, c("a", "b", "c")); print(names(short)); print(s$b)
d <- `names<-`(dbl, nm); print(is.null(names(dbl))); d[1] <- -1; print(dbl[1]); print(d[1:2])
a <- `attr<-`(long, "tag", "x"); print(attr(long, "tag")); print(attr(a, "tag"))
cl <- `class<-`(long, "thing"); print(class(long)); print(inherits(cl, "thing"))
at <- `attributes<-`(long, list(names = nm)); print(is.null(names(long))); print(names(at)[1:2])
str(lapply(named[1:3], function(v) v))
print(sum(unlist(`names<-`(long, nm))))
