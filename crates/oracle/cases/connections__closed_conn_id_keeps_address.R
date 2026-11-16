# Closing a connection leaves its conn_id address in place, so packages that compare
# connection ids (future, parallelly) can tell a closed connection from a serialized one.
address <- function(con) capture.output(print(attr(con, "conn_id")))
con <- textConnection("x")
close(con)
!address(con) %in% c("<pointer: (nil)>", "<pointer: 0x0>")
con <- file(tempfile(), "w")
before <- address(con)
close(con)
identical(address(con), before)
