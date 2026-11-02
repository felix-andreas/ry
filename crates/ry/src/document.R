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

  # roxygen2's Rd roclet takes time quadratic in the number of topics in several places, which
  # is most of a run on a package with thousands of them: it grows its named list of topics one
  # element at a time (each append copies the list), and it finds topics by name, which scans the
  # list, once per topic in `topo_apply` (fifteen times over for `@inherit`), in `topo_order`
  # and in `drop_invalid`. Each replacement computes exactly what the original does in linear
  # time, and replaces it only while the original's body is the one written here, so another
  # roxygen2 version runs unchanged. None of roxygen2's inherit steps adds or removes topics,
  # which is what lets `topo_apply` look the topics up in one pass.
  replacements <- list(
    list(
      name = "roclet_process.roclet_rd",
      original = quote({
        blocks <- merge_external_r6methods(blocks)
        topics <- RoxyTopics$new()
        for (block in blocks) {
          rd <- block_to_rd(block, base_path, env)
          topics$add(rd, block)
        }
        topics_process_r6_inherit(topics)
        topics_process_family(topics, env)
        topics_process_inherit(topics, env)
        topics$drop_invalid()
        topics_fix_params_order(topics)
        topics_add_default_description(topics)
        topics_add_package_alias(topics)
        topics$topics
      }),
      replacement = function(x, blocks, env, base_path) {
        blocks <- merge_external_r6methods(blocks)
        topics <- RoxyTopics$new()
        # `topics$add` for every block, with the list built in preallocated vectors and each
        # file found through a hashed index; the first name a file cannot be an index key for
        # hands the rest of the blocks to `topics$add` itself.
        collected <- vector("list", length(blocks))
        files <- character(length(blocks))
        positions <- new.env(hash = TRUE, parent = emptyenv())
        count <- 0L
        indexed <- TRUE
        for (block in blocks) {
          rd <- block_to_rd(block, base_path, env)
          if (!indexed) {
            topics$add(rd, block)
            next
          }
          if (is.null(rd)) next
          stopifnot(inherits(rd, "RoxyTopic"))
          file <- rd$filename
          if (!is.character(file) || length(file) != 1L || is.na(file) || !nzchar(file)) {
            topics$topics <- stats::setNames(collected[seq_len(count)], files[seq_len(count)])
            indexed <- FALSE
            topics$add(rd, block)
            next
          }
          position <- positions[[file]]
          if (is.null(position)) {
            count <- count + 1L
            collected[[count]] <- rd
            files[[count]] <- file
            positions[[file]] <- count
          } else {
            collected[[position]]$add(rd, block)
          }
        }
        if (indexed) {
          topics$topics <- stats::setNames(collected[seq_len(count)], files[seq_len(count)])
        }
        topics_process_r6_inherit(topics)
        topics_process_family(topics, env)
        topics_process_inherit(topics, env)
        topics$drop_invalid()
        topics_fix_params_order(topics)
        topics_add_default_description(topics)
        topics_add_package_alias(topics)
        topics$topics
      }
    ),
    list(
      method = "topo_apply",
      original = quote({
        topics_topo <- self$topo_order(dep)
        for (topic_name in topics_topo) {
          topic <- self$get(topic_name)
          fun(topic, self, ...)
        }
        invisible()
      }),
      replacement = function(dep, fun, ...) {
        topics_topo <- self$topo_order(dep)
        topics <- self$topics
        positions <- match(topics_topo, names(topics))
        for (position in positions) {
          topic <- if (is.na(position)) NULL else topics[[position]]
          fun(topic, self, ...)
        }
        invisible()
      }
    ),
    list(
      method = "drop_invalid",
      original = quote({
        for (topic in names(self$topics)) {
          if (!self$topics[[topic]]$is_valid()) {
            warn_roxy_topic(topic, "Skipping; no name and/or title")
            self$topics[[topic]] <- NULL
          }
        }
        invisible()
      }),
      replacement = function() {
        valid <- vapply(self$topics, function(topic) topic$is_valid(), logical(1L))
        for (topic in names(self$topics)[!valid]) {
          warn_roxy_topic(topic, "Skipping; no name and/or title")
        }
        if (!all(valid)) self$topics <- self$topics[valid]
        invisible()
      }
    ),
    list(
      method = "topo_order",
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
    methods <- roxygen$.__S3MethodsTable__.
    replaced <- list()
    for (replacement in replacements) {
      expected <- deparse(replacement$original)
      if (is.null(replacement$method)) {
        name <- replacement$name
        current <- roxygen[[name]]
        if (is.function(current) && identical(deparse(body(current)), expected) &&
            identical(methods[[name]], current)) {
          environment(replacement$replacement) <- roxygen
          unlockBinding(name, roxygen)
          assign(name, replacement$replacement, envir = roxygen)
          assign(name, replacement$replacement, envir = methods)
          replaced[[name]] <- list(original = current, method = FALSE)
        }
      } else {
        name <- replacement$method
        current <- topics$public_methods[[name]]
        if (is.function(current) && identical(deparse(body(current)), expected)) {
          topics$set("public", name, replacement$replacement, overwrite = TRUE)
          replaced[[name]] <- list(original = current, method = TRUE)
        }
      }
    }
    on.exit(for (name in names(replaced)) {
      previous <- replaced[[name]]$original
      if (replaced[[name]]$method) {
        topics$set("public", name, previous, overwrite = TRUE)
      } else {
        assign(name, previous, envir = roxygen)
        assign(name, previous, envir = methods)
        lockBinding(name, roxygen)
      }
    }, add = TRUE)
    if (nzchar(Sys.getenv("RY_DOCUMENT_DEBUG"))) message("replaced: ", toString(names(replaced)))
    roxygen2::roxygenise(root, roclets, load_code = load_code)
    invisible(TRUE)
  }
})
