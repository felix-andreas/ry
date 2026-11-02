local({
  roxygen <- asNamespace("roxygen2")
  internal <- function(name) get(name, envir = roxygen)

  # roxygen2 spends most of a run tokenizing every block of every file, and a block's tokens
  # depend only on its file's text and on the package's roxygen options. So this replaces
  # `roxygen2:::parse_package` for one run with a version that reuses the blocks of each
  # unchanged file from the last run, which `cache` keeps on disk.
  cached_parse_package <- function(cache) {
    package_files <- internal("package_files")
    tokenize_file <- internal("tokenize_file")
    block_set_env <- internal("block_set_env")
    order_blocks <- internal("order_blocks")
    meta <- internal("roxy_meta")
    function(path = ".", env = NULL) {
      options <- mget(sort(setdiff(ls(meta, all.names = TRUE), "env")), envir = meta)
      key <- list(utils::packageVersion("roxygen2"), R.version$version.string, options)
      previous <- if (length(cache) && file.exists(cache)) {
        tryCatch(readRDS(cache), error = function(error) NULL)
      }
      if (!identical(previous$key, key)) previous <- NULL
      files <- package_files(path)
      sums <- unname(tools::md5sum(files))
      entries <- vector("list", length(files))
      names(entries) <- files
      reused <- 0L
      for (i in seq_along(files)) {
        entry <- previous$files[[files[[i]]]]
        if (!is.null(entry) && identical(entry$md5, sums[[i]])) {
          reused <- reused + 1L
        } else {
          entry <- list(md5 = sums[[i]], blocks = tokenize_file(files[[i]]))
        }
        entries[[i]] <- entry
      }
      if (length(cache) && (reused < length(files) || length(previous$files) != length(files))) {
        dir.create(dirname(cache), recursive = TRUE, showWarnings = FALSE)
        partial <- tempfile(tmpdir = dirname(cache))
        saved <- tryCatch({ saveRDS(list(key = key, files = entries), partial); TRUE },
                          error = function(error) FALSE)
        if (saved) file.rename(partial, cache) else unlink(partial)
      }
      blocks <- unlist(lapply(entries, `[[`, "blocks"), recursive = FALSE, use.names = FALSE)
      if (!is.null(env)) blocks <- lapply(blocks, block_set_env, env = env)
      order_blocks(blocks)
    }
  }

  # Two methods of roxygen2's topic collection take time quadratic in the number of topics,
  # which is most of a run on a package with thousands of them: `topo_order`, which runs once
  # per inherited field, builds its graph in lists that grow by copying, and `add` matches the
  # new topic's file against every name. Each replacement computes exactly what the original
  # does in linear time, and replaces it only while its body is the one written here, so
  # another roxygen2 version keeps its own.
  topic_methods <- list(
    topo_order = list(
      original = quote({
        topo <- TopoSort$new()
        for (i in seq_along(self$topics)) {
          name <- names(self$topics)[[i]]
          topo$add(name)
          dep_topics <- dependencies(self$topics[[i]])
          for (dep_topic in dep_topics) {
            dep_rd <- self$find_filename(dep_topic)
            if (!is.na(dep_rd)) {
              topo$add_ancestor(name, dep_rd)
            }
          }
        }
        topo$sort()
      }),
      replacement = function(dependencies) {
        topics <- self$topics
        files <- names(topics)
        # `find_filename` returns the first topic that has the name, so the index keeps the
        # first; a name an environment cannot hold falls back to it.
        first <- NULL
        find <- function(name) {
          if (!is.character(name) || length(name) != 1L || is.na(name) || !nzchar(name)) {
            return(self$find_filename(name))
          }
          if (is.null(first)) {
            first <<- new.env(hash = TRUE, parent = emptyenv())
            for (i in rev(seq_along(topics))) {
              for (alias in topics[[i]]$get_value("name")) {
                if (is.character(alias) && !is.na(alias) && nzchar(alias)) {
                  assign(alias, files[[i]], envir = first)
                }
              }
            }
          }
          get0(name, envir = first, inherits = FALSE, ifnotfound = NA_character_)
        }
        ids <- new.env(hash = TRUE, parent = emptyenv())
        vertices <- character(length(files))
        ancestors <- vector("list", length(files))
        count <- 0L
        add <- function(name) {
          id <- ids[[name]]
          if (is.null(id)) {
            count <<- count + 1L
            vertices[[count]] <<- name
            ids[[name]] <- count
            id <- count
          }
          id
        }
        for (i in seq_along(topics)) {
          id <- add(files[[i]])
          for (dependency in dependencies(topics[[i]])) {
            file <- find(dependency)
            if (!is.na(file)) {
              ancestor <- add(file)
              # A vertex keeps its ancestors newest first and each once.
              if (!(ancestor %in% ancestors[[id]])) {
                ancestors[[id]] <- c(ancestor, ancestors[[id]])
              }
            }
          }
        }
        discovered <- logical(count)
        sorted <- character(count)
        done <- 0L
        visit <- function(vertex) {
          discovered[[vertex]] <<- TRUE
          for (ancestor in ancestors[[vertex]]) {
            if (!discovered[[ancestor]]) visit(ancestor)
          }
          done <<- done + 1L
          sorted[[done]] <<- vertices[[vertex]]
        }
        for (vertex in seq_len(count)) {
          if (!discovered[[vertex]]) visit(vertex)
        }
        sorted
      }
    ),
    add = list(
      original = quote({
        if (is.null(topic)) {
          return()
        }
        stopifnot(inherits(topic, "RoxyTopic"))
        filename <- topic$filename
        if (filename %in% names(self$topics)) {
          self$topics[[filename]]$add(topic, block)
        } else {
          self$topics[[filename]] <- topic
        }
        invisible()
      }),
      replacement = function(topic, block) {
        if (is.null(topic)) {
          return()
        }
        stopifnot(inherits(topic, "RoxyTopic"))
        filename <- topic$filename
        # `[[` finds the first topic of that name, as the original's `%in%` does, without
        # hashing every name; a name `[[` cannot look up takes the original's path.
        if (!is.character(filename) || length(filename) != 1L || is.na(filename)) {
          if (filename %in% names(self$topics)) {
            self$topics[[filename]]$add(topic, block)
          } else {
            self$topics[[filename]] <- topic
          }
        } else if (!is.null(existing <- self$topics[[filename]])) {
          existing$add(topic, block)
        } else {
          self$topics[[filename]] <- topic
        }
        invisible()
      }
    )
  )

  function(root, roclets, cache) {
    package <- read.dcf(file.path(root, "DESCRIPTION"), fields = "Package")[[1L]]
    message("ℹ Updating ", package, " documentation")
    load_code <- function(path) {
      load_all(path, quiet = TRUE)
      asNamespace(package)
    }
    original <- roxygen$parse_package
    if (identical(names(formals(original)), c("path", "env"))) {
      unlockBinding("parse_package", roxygen)
      assign("parse_package", cached_parse_package(cache), envir = roxygen)
      on.exit({
        assign("parse_package", original, envir = roxygen)
        lockBinding("parse_package", roxygen)
      })
    }
    topics <- roxygen$RoxyTopics
    replaced <- list()
    for (name in names(topic_methods)) {
      method <- topics$public_methods[[name]]
      if (is.function(method) &&
          identical(deparse(body(method)), deparse(topic_methods[[name]]$original))) {
        replaced[[name]] <- method
        topics$set("public", name, topic_methods[[name]]$replacement, overwrite = TRUE)
      }
    }
    on.exit(for (name in names(replaced)) {
      topics$set("public", name, replaced[[name]], overwrite = TRUE)
    }, add = TRUE)
    if (nzchar(Sys.getenv("RY_DOCUMENT_DEBUG"))) message("replaced: ", toString(names(replaced)))
    roxygen2::roxygenise(root, roclets, load_code = load_code)
    invisible(TRUE)
  }
})
