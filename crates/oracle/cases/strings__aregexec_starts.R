# A match that starts earlier at a higher cost occupies the position a cheaper
# match would start from, so TRE keeps the earlier start: `x+` is one pattern
# position with a loop, and `x{2,}` is two copies followed by `x*`.
show <- function(pattern, text, ...) {
    m <- aregexec(pattern, text, ...)
    print(m)
    print(regmatches(text, m))
}
show("([a-c]+)(d)(e?)", c("xbcdy", "aXcde", "bcd", "zzbcdee"), max.distance = 1)
show("[a-c]+d", "xbcdy", max.distance = 1)
show("[a-c]*d", "xbcdy", max.distance = 1)
show("[a-c]{2,}d", c("xbcdy", "xybcd"), max.distance = 1)
show("[a-c]{1,3}d", c("xbcdy", "xabcd"), max.distance = 1)
show("(ab)+c", c("xabababc", "abXbc"), max.distance = 1)
show("qu(i|a)ck br[a-z]+n", c("the quick brown fox", "a quack brawn", "qick brn"), max.distance = 2)
