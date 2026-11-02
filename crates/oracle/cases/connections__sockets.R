# Socket connections between a server socket, a client and the accepted peer, all in one
# process. Each read first waits in socketSelect(), so the output does not depend on timing.
port <- 41873L
server <- serverSocket(port)
print(class(server))
print(unlist(summary(server)))

client <- socketConnection("127.0.0.1", port, open = "a+")
print(socketSelect(list(server), timeout = 5))
peer <- socketAccept(server, open = "a+")
print(class(client))
print(summary(client)$description)
print(summary(peer)$description)
print(summary(client)$mode)
print(c(isOpen(client), isOpen(peer)))

# Nothing to read yet: a non-blocking read is empty and incomplete.
print(socketSelect(list(client, peer), timeout = 0))
print(readLines(peer))
print(isIncomplete(peer))
print(socketSelect(list(client, peer), write = TRUE, timeout = 0))

# A partial line waits until its newline arrives.
cat("partial", file = client)
print(socketSelect(list(peer), timeout = 5))
print(readLines(peer))
cat(" line\nnext\n", file = client)
print(socketSelect(list(peer), timeout = 5))
print(readLines(peer))
writeLines(c("reply", "two"), peer)
print(socketSelect(list(client), timeout = 5))
print(readLines(client))

# Timeouts: the old value comes back, and a negative one only asks.
print(socketTimeout(client, 5))
print(socketTimeout(client))

# Errors.
message <- function(expr) tryCatch(expr, error = conditionMessage, warning = conditionMessage)
print(message(socketSelect(list())))
print(message(socketSelect(list(1))))
print(message(socketSelect(list(stdin()))))
print(message(socketAccept(stdin())))
print(message(socketTimeout(stdin())))
print(message(socketTimeout(server)))
print(message(serverSocket(-1)))
print(message(serverSocket(port)))
print(message(socketConnection(host = 1, port = port)))
print(message(readLines(server)))
print(message(writeLines("x", server)))

# The peer closing ends the input, though writing still succeeds.
close(client)
print(socketSelect(list(peer), timeout = 5))
print(readLines(peer))
close(peer)
close(server)
print(message(socketConnection("127.0.0.1", port, open = "a+")))
