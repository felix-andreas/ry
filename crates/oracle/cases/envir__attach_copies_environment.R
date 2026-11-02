# attach(env) attaches a copy of the bindings, so the original keeps its enclosure and later
# changes to it do not show through.
e <- new.env()
assign("v", 1, envir = e)
original_parent <- parent.env(e)
attach(e, name = "copied")
assign("v", 2, envir = e)
print(get("v", pos = "copied"))
print(get("v", envir = as.environment("copied")))
print(identical(parent.env(e), original_parent))
detach("copied")
