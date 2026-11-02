# stdout() names the connection output currently goes to, so inside a sink it is the sink,
# while a stdout() kept from before the sink writes past it to the console.
console <- stdout()
tc <- textConnection("captured", "w")
sink(tc)
cat("past the sink\n", file = console)
cat("into the sink\n")
cat("also into the sink\n", file = stdout())
sink()
close(tc)
print(captured)
