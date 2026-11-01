# The ry runtime backend of the loader's R side. The plan stays in the ry process, and the
# interpreter lowers each definition and statement straight from the analysis's syntax tree,
# so no source text reaches R and nothing is parsed twice.

.ry_request <- function(root, full) .Internal(ryLoaderPlan(root, full))

.ry_commit <- function(root) invisible(.Internal(ryLoaderCommit(root)))

.ry_sources <- function(state, plan) invisible()

.ry_bind_lazily <- function(state, plan, keep) .Internal(ryLoaderBind(state$root, state$ns))

.ry_run <- function(state, plan, i, keep) .Internal(ryLoaderRun(state$root, state$ns, i))
