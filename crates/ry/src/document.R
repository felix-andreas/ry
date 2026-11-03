local({
  roxygen <- asNamespace("roxygen2")
  internal <- function(name) get(name, envir = roxygen)

  # roxygen2 spends much of a run tokenizing every block of every file, and a block's tokens
  # depend only on its file's text and on the package's roxygen options. So this replaces
  # `roxygen2:::parse_package` for one run with a version that reuses the blocks of each
  # unchanged file from the last run, which `cache` keeps on disk. Each block gets an id, its
  # file and position, which the Rd and namespace caches below key on.
  cached_parse_package <- function(cache, state) {
    package_files <- internal("package_files")
    tokenize_file <- internal("tokenize_file")
    block_set_env <- internal("block_set_env")
    order_blocks <- internal("order_blocks")
    function(path = ".", env = NULL) {
      key <- roxygen_key()
      state$key <- key
      previous <- if (length(cache) && file.exists(cache)) {
        tryCatch(readRDS(cache), error = function(error) NULL)
      }
      if (!identical(previous$key, key)) previous <- NULL
      files <- package_files(path)
      sums <- unname(tools::md5sum(files))
      entries <- vector("list", length(files))
      names(entries) <- files
      reused <- logical(length(files))
      for (i in seq_along(files)) {
        entry <- previous$files[[files[[i]]]]
        if (!is.null(entry) && identical(entry$md5, sums[[i]])) {
          reused[[i]] <- TRUE
        } else {
          entry <- list(md5 = sums[[i]], blocks = tokenize_file(files[[i]]))
        }
        entries[[i]] <- entry
      }
      if (length(cache) && (!all(reused) || length(previous$files) != length(files))) {
        save_atomically(list(key = key, files = entries), cache)
      }
      state$unchanged_files <- files[reused]
      blocks <- unlist(Map(function(entry, file) {
        Map(function(block, position) {
          block$ry_id <- paste0(file, "#", position)
          block
        }, entry$blocks, seq_along(entry$blocks))
      }, entries, files), recursive = FALSE, use.names = FALSE)
      if (!is.null(env)) blocks <- lapply(blocks, block_set_env, env = env)
      order_blocks(blocks)
    }
  }

  roxygen_key <- function() {
    meta <- internal("roxy_meta")
    options <- mget(sort(setdiff(ls(meta, all.names = TRUE), "env")), envir = meta)
    list(utils::packageVersion("roxygen2"), R.version$version.string, options)
  }

  # Before loading the package, roxygen2 parses every file again to find its import directives
  # and writes them to NAMESPACE. A file's directives depend only on its text, so this reuses
  # those of each unchanged file, kept in `cache`.
  cached_namespace_imports <- function(cache) {
    package_files <- internal("package_files")
    imports_blocks <- internal("namespace_imports_blocks")
    block_directives <- internal("block_directives")
    compact <- internal("compact")
    list_c <- internal("list_c")
    function(base_path = ".") {
      key <- roxygen_key()
      previous <- if (file.exists(cache)) tryCatch(readRDS(cache), error = function(error) NULL)
      if (!identical(previous$key, key)) previous <- NULL
      paths <- package_files(base_path)
      sums <- unname(tools::md5sum(paths))
      reused <- 0L
      entries <- Map(function(path, sum) {
        entry <- previous$files[[path]]
        if (!is.null(entry) && identical(entry$md5, sum)) {
          reused <<- reused + 1L
          return(entry)
        }
        srcref <- utils::getSrcref(parse(path, keep.source = TRUE))
        blocks <- suppressMessages(imports_blocks(srcref))
        list(md5 = sum, directives = block_directives(blocks, emptyenv()))
      }, paths, sums)
      if (reused < length(paths) || length(previous$files) != length(paths)) {
        save_atomically(list(key = key, files = entries), cache)
      }
      compact(list_c(unname(lapply(entries, `[[`, "directives"))))
    }
  }

  save_atomically <- function(value, path) {
    dir.create(dirname(path), recursive = TRUE, showWarnings = FALSE)
    partial <- tempfile(tmpdir = dirname(path))
    saved <- tryCatch({ saveRDS(value, partial); TRUE }, error = function(error) FALSE)
    if (saved) file.rename(partial, path) else unlink(partial)
  }

  # A run after an edit rebuilds only the Rd files the edit can change and reuses the rest from
  # the last run, which `cache` keeps beside the parsed blocks. A block is cached when it
  # documents a plain function defined right there (`name <- function(...)`), because then its
  # Rd and NAMESPACE output depend on nothing but its own unchanged file: the namespace binding
  # is checked to be that very definition through its source reference, a name without a dot
  # cannot turn into an S3 method when another file adds a generic, and the tags that read other
  # files or the namespace (`uncacheable_tags`) are excluded. Topics then depend on each other
  # only by name, through `@inherit*` and `@family`, and on every topic's aliases for the package
  # topic. So a run rebuilds every topic with a block that is not cached, then every topic that
  # inherits from one of those or shares a family with one, transitively, and computes them from
  # their blocks plus the blocks of every topic they read. Everything else is written from the
  # cache, and not at all while its file on disk is the one the last run left.
  uncacheable_tags <- c(
    "eval", "evalRd", "evalNamespace", "template", "templateVar", "example", "includeRmd",
    "describeIn", "R6method", "method", "exportS3Method"
  )

  # The function `block` documents, while the namespace's binding is the definition in the
  # block's own source: its source reference names the block's file and line.
  defined_here <- function(block, env) {
    call <- block$call
    if (!is.call(call) || length(call) != 3L || !is.name(call[[2L]])) return(NULL)
    if (!identical(call[[1L]], as.name("<-")) && !identical(call[[1L]], as.name("="))) return(NULL)
    definition <- call[[3L]]
    if (!is.call(definition) || !identical(definition[[1L]], as.name("function"))) return(NULL)
    name <- as.character(call[[2L]])
    if (grepl(".", name, fixed = TRUE)) return(NULL)
    value <- get0(name, envir = env)
    if (typeof(value) != "closure" || !identical(class(value), "function")) return(NULL)
    source <- attr(value, "srcref")
    if (length(source) < 1L || !isTRUE(source[[1L]] == block$line)) return(NULL)
    file <- attr(source, "srcfile")
    if (!is.environment(file) || !identical(file$filename, block$file)) return(NULL)
    value
  }

  # roxygen2's `block_set_env`, which also records whether the block can be cached.
  evaluate_block <- function(block, env) {
    block$ry_clean <- NULL
    block$object <- NULL
    id <- block$ry_id
    cacheable <- !is.null(id) && !any(block_tags(block) %in% uncacheable_tags)
    block <- block_evaluate(block, env)
    block <- block_find_object(block, env)
    if (cacheable && identical(class(block$object)[[1L]], "function") &&
        !is.null(defined_here(block, env)) && !exists(id, envir = state$entries, inherits = FALSE)) {
      assign(id, list(), envir = state$entries)
    }
    block
  }

  load_topics <- function(path, key) {
    saved <- if (length(path) && file.exists(path)) {
      tryCatch(readRDS(path), error = function(error) NULL)
    }
    if (is.null(saved) || !identical(saved$key, key)) return(NULL)
    list(
      names = names(saved$topics),
      topics = list2env(saved$topics, hash = TRUE),
      blocks = list2env(saved$blocks, hash = TRUE)
    )
  }

  topic_dependencies <- function(topic) {
    unique(c(
      topic$get_value("inherit")$source,
      topic$get_value("inherit_section")$source,
      topic$get_value("inherit_dot_params")$source
    ))
  }

  # The topics a run must rebuild (`rebuild`), the cached blocks it needs to compute them
  # (`blocks`), and the topics it writes from the cache (`kept`). `rds` are the Rd fragments of
  # the blocks that are not cached.
  plan_topics <- function(previous, rds, clean_ids) {
    topics <- previous$names
    old <- mget(topics, envir = previous$topics)
    field <- function(records, name) lapply(records, `[[`, name)
    index <- function(records, name) {
      values <- field(records, name)
      keys <- as.character(unlist(values, use.names = FALSE))
      index <- new.env(hash = TRUE, parent = emptyenv())
      if (length(keys)) {
        owners <- rep(names(records), lengths(values))
        usable <- !is.na(keys) & nzchar(keys)
        list2env(split(owners[usable], keys[usable]), envir = index)
      }
      index
    }
    fresh <- list()
    for (rd in rds) {
      if (is.null(rd)) next
      file <- rd$filename
      current <- fresh[[file]]
      fresh[[file]] <- list(
        keys = c(current$keys, rd$get_value("name"), rd$get_value("alias")),
        dependencies = c(current$dependencies, topic_dependencies(rd)),
        families = c(current$families, rd$get_value("family")),
        package = isTRUE(current$package) || identical(rd$get_value("docType"), "package")
      )
    }
    by_key <- list(index(old, "keys"), index(fresh, "keys"))
    by_family <- list(index(old, "families"), index(fresh, "families"))
    by_dependency <- index(old, "dependencies")
    lookup <- function(indexes, keys) {
      keys <- as.character(keys)
      keys <- unique(keys[!is.na(keys) & nzchar(keys)])
      if (!length(keys)) return(character())
      unlist(lapply(indexes, function(index) mget(keys, envir = index, ifnotfound = list(NULL))),
             use.names = FALSE)
    }
    facts <- function(topic, name) {
      c(get0(topic, envir = previous$topics, inherits = FALSE)[[name]], fresh[[topic]][[name]])
    }
    members <- field(old, "blocks")
    owners <- rep(seq_along(old), lengths(members))
    stale <- owners[!(unlist(members, use.names = FALSE) %in% clean_ids)]
    flagged <- function(name) topics[vapply(old, function(record) isTRUE(record[[name]]), logical(1L))]
    start <- c(topics[stale], topics[!topics %in% flagged("cacheable")], flagged("package"),
               names(fresh))
    rebuild <- closure(start, function(topic) {
      c(lookup(list(by_dependency), facts(topic, "keys")),
        lookup(by_family, facts(topic, "families")))
    })
    needed <- closure(rebuild, function(topic) {
      c(lookup(by_key, facts(topic, "dependencies")), lookup(by_family, facts(topic, "families")))
    })
    list(
      rebuild = rebuild,
      blocks = unlist(field(old[topics %in% needed], "blocks"), use.names = FALSE),
      kept = topics[!topics %in% needed]
    )
  }

  # Every name reachable from `start` through `step`, in the order found.
  closure <- function(start, step) {
    seen <- new.env(hash = TRUE, parent = emptyenv())
    found <- character(max(16L, 2L * length(start)))
    count <- 0L
    visit <- function(names) {
      for (name in names) {
        if (is.na(name) || exists(name, envir = seen, inherits = FALSE)) next
        assign(name, TRUE, envir = seen)
        if (count == length(found)) found <<- c(found, character(length(found)))
        count <<- count + 1L
        found[[count]] <<- name
      }
    }
    visit(unique(start))
    position <- 0L
    while (position < count) {
      position <- position + 1L
      visit(step(found[[position]]))
    }
    found[seq_len(count)]
  }

  # A topic as `roclet_output` reads one: its text and its name, from the cache.
  topic_stub <- function(record) {
    stub <- new.env(parent = emptyenv())
    text <- record$text
    name <- record$name
    stub$format <- function(...) text
    stub$get_name <- function() name
    class(stub) <- "R6"
    stub
  }

  # roxygen2's `topics_add_package_alias`, which also counts the aliases of the topics a run
  # does not rebuild.
  add_package_alias <- function(topics, cached_aliases) {
    aliases <- c(cached_aliases, unlist(topics$simple_values("alias"), use.names = FALSE))
    for (topic in topics$topics) {
      if (!identical(topic$get_value("docType"), "package")) {
        next
      }
      package <- topic$get_value("package")
      defaults <- c(package, package_suffix(package))
      aliases <- union(setdiff(defaults, aliases), topic$get_value("alias"))
      topic$add(rd_section("alias", aliases), overwrite = TRUE)
      break
    }
    invisible(NULL)
  }

  # The functions the incremental run reimplements or relies on keeping their meaning; any other
  # body turns it off.
  expected <- list(
    topics_add_package_alias = quote({
      aliases <- unlist(topics$simple_values("alias"), use.names = FALSE)
      for (topic in topics$topics) {
        if (!identical(topic$get_value("docType"), "package")) {
          next
        }
        package <- topic$get_value("package")
        defaults <- c(package, package_suffix(package))
        aliases <- union(setdiff(defaults, aliases), topic$get_value("alias"))
        topic$add(rd_section("alias", aliases), overwrite = TRUE)
        break
      }
      invisible(NULL)
    }),
    blocks_to_ns = quote({
      ns_format(block_directives(blocks, env))
    }),
    block_directives = quote({
      directives <- map(blocks, function(block) {
        map(block$tags, roxy_tag_ns, block = block, env = env)
      })
      compact(list_c(directives))
    })
  )

  helpers <- c("uncacheable_tags", "defined_here", "evaluate_block", "topic_dependencies",
               "plan_topics", "closure", "topic_stub", "add_package_alias")

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
      name = "block_set_env",
      original = quote({
        block <- block_evaluate(block, env)
        block <- block_find_object(block, env)
        block
      }),
      replacement = function(block, env) {
        id <- block$ry_id
        cached <- if (!is.null(id) && !is.null(state$previous)) {
          get0(id, envir = state$previous$blocks, inherits = FALSE)
        }
        if (!is.null(cached) && exists(block$file, envir = state$unchanged, inherits = FALSE)) {
          value <- defined_here(block, env)
          if (!is.null(value)) {
            # `warn_missing_s3_exports` reads the value of every exported block.
            block$object <- list(value = value)
            block$ry_clean <- TRUE
            assign(id, cached, envir = state$entries)
            return(block)
          }
        }
        evaluate_block(block, env)
      }
    ),
    list(
      name = "roclet_process.roclet_namespace",
      s3 = TRUE,
      original = quote({
        warn_missing_s3_exports(blocks, env)
        blocks_to_ns(blocks, env)
      }),
      replacement = function(x, blocks, env, base_path) {
        warn_missing_s3_exports(blocks, env)
        directives <- lapply(blocks, function(block) {
          id <- block$ry_id
          entry <- if (!is.null(id)) get0(id, envir = state$entries, inherits = FALSE)
          if (isTRUE(block$ry_clean)) {
            if (!is.null(entry$namespace)) return(entry$namespace[[1L]])
            block <- evaluate_block(block, env)
          }
          found <- compact(map(block$tags, roxy_tag_ns, block = block, env = env))
          if (!is.null(entry)) {
            entry$namespace <- list(found)
            assign(id, entry, envir = state$entries)
          }
          found
        })
        ns_format(compact(list_c(directives)))
      }
    ),
    list(
      name = "roclet_process.roclet_rd",
      s3 = TRUE,
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
        count <- length(blocks)
        ids <- vapply(blocks, function(block) block$ry_id %||% NA_character_, character(1L))
        clean <- vapply(blocks, function(block) isTRUE(block$ry_clean), logical(1L))
        rds <- vector("list", count)
        for (i in which(!clean)) rds[i] <- list(block_to_rd(blocks[[i]], base_path, env))
        selected <- !clean
        plan <- NULL
        if (!is.null(state$previous)) {
          plan <- plan_topics(state$previous, rds[!clean], ids[clean])
          needed <- clean & ids %in% plan$blocks
          for (i in which(needed)) {
            blocks[[i]] <- evaluate_block(blocks[[i]], env)
            rds[i] <- list(block_to_rd(blocks[[i]], base_path, env))
          }
          selected <- selected | needed
        }
        # `topics$add` for every block, with the list built in preallocated vectors and each
        # file found through a hashed index.
        collected <- vector("list", count)
        files <- character(count)
        owners <- vector("list", count)
        positions <- new.env(hash = TRUE, parent = emptyenv())
        found <- 0L
        for (i in which(selected)) {
          rd <- rds[[i]]
          if (is.null(rd)) next
          stopifnot(inherits(rd, "RoxyTopic"))
          file <- rd$filename
          position <- positions[[file]]
          if (is.null(position)) {
            found <- found + 1L
            collected[[found]] <- rd
            files[[found]] <- file
            owners[[found]] <- ids[[i]]
            positions[[file]] <- found
          } else {
            collected[[position]]$add(rd, blocks[[i]])
            owners[[position]] <- c(owners[[position]], ids[[i]])
          }
        }
        topics <- RoxyTopics$new()
        topics$topics <- stats::setNames(collected[seq_len(found)], files[seq_len(found)])
        topics_process_r6_inherit(topics)
        topics_process_family(topics, env)
        topics_process_inherit(topics, env)
        topics$drop_invalid()
        topics_fix_params_order(topics)
        topics_add_default_description(topics)
        cached <- if (!is.null(plan)) {
          mget(plan$kept, envir = state$previous$topics)
        }
        add_package_alias(topics, unlist(lapply(cached, `[[`, "aliases"), use.names = FALSE))
        if (!state$incremental) return(topics$topics)
        owners <- stats::setNames(owners[seq_len(found)], files[seq_len(found)])
        # A topic roxygen2 drops is rebuilt every run, so that it reappears once whatever made
        # it invalid is fixed.
        for (file in setdiff(names(owners), names(topics$topics))) {
          members <- owners[[file]]
          members <- members[vapply(members, exists, logical(1L), envir = state$entries,
                                    inherits = FALSE)]
          rm(list = members, envir = state$entries)
        }
        records <- lapply(stats::setNames(nm = names(topics$topics)), function(file) {
          topic <- topics$topics[[file]]
          members <- owners[[file]]
          list(
            text = topic$format(),
            name = topic$get_name(),
            keys = unique(c(topic$get_value("name"), topic$get_value("alias"))),
            aliases = topic$get_value("alias"),
            dependencies = topic_dependencies(topic),
            families = topic$get_value("family"),
            package = identical(topic$get_value("docType"), "package"),
            blocks = members,
            cacheable = all(vapply(members, exists, logical(1L), envir = state$entries,
                                   inherits = FALSE))
          )
        })
        if (length(cached)) {
          man <- file.path(base_path, "man", names(cached))
          info <- file.info(man, extra_cols = FALSE)
          size <- vapply(cached, function(record) record$size %||% NA_real_, numeric(1L))
          mtime <- vapply(cached, function(record) record$mtime %||% NA_real_, numeric(1L))
          same <- !is.na(info$size) & info$size == size & as.numeric(info$mtime) == mtime
          state$skip <- list2env(as.list(stats::setNames(same, names(cached))), hash = TRUE)
        }
        if (nzchar(Sys.getenv("RY_DOCUMENT_DEBUG"))) {
          message("rebuilt ", length(records), " topics, kept ", length(cached))
        }
        state$records <- c(records, cached)
        lapply(state$records, topic_stub)
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
    state <- new.env(parent = emptyenv())
    state$entries <- new.env(hash = TRUE, parent = emptyenv())
    state$unchanged <- new.env(hash = TRUE, parent = emptyenv())
    state$skip <- emptyenv()
    state$incremental <- FALSE
    run <- new.env(parent = roxygen)
    run$state <- state
    for (name in helpers) {
      helper <- get(name)
      if (is.function(helper)) environment(helper) <- run
      assign(name, helper, envir = run)
    }
    topic_cache <- if (length(cache)) paste0(cache, "-topics")
    original <- roxygen$parse_package
    parse_replaced <- identical(names(formals(original)), c("path", "env"))
    if (parse_replaced) {
      parse <- cached_parse_package(cache, state)
      unlockBinding("parse_package", roxygen)
      assign("parse_package", function(path = ".", env = NULL) {
        blocks <- parse(path, env)
        for (file in state$unchanged_files) assign(file, TRUE, envir = state$unchanged)
        chosen <- roclets
        if (is.null(chosen)) chosen <- roxygen$roxy_meta_get("roclets")
        # A roclet other than these reads blocks the run leaves unevaluated.
        if (state$incremental && all(chosen %in% c("collate", "namespace", "rd"))) {
          state$previous <- load_topics(topic_cache, state$key)
        }
        blocks
      }, envir = roxygen)
      on.exit({
        assign("parse_package", original, envir = roxygen)
        lockBinding("parse_package", roxygen)
      })
    }
    imports_original <- roxygen$namespace_imports
    if (length(cache) && is.function(imports_original) &&
        identical(deparse(body(imports_original)), deparse(quote({
          paths <- package_files(base_path)
          parsed <- lapply(paths, parse, keep.source = TRUE)
          srcrefs <- lapply(parsed, utils::getSrcref)
          suppressMessages(blocks <- unlist(lapply(srcrefs, namespace_imports_blocks),
                                            recursive = FALSE))
          block_directives(blocks, emptyenv())
        })))) {
      unlockBinding("namespace_imports", roxygen)
      assign("namespace_imports", cached_namespace_imports(paste0(cache, "-imports")),
             envir = roxygen)
      on.exit({
        assign("namespace_imports", imports_original, envir = roxygen)
        lockBinding("namespace_imports", roxygen)
      }, add = TRUE)
    }
    topics <- roxygen$RoxyTopics
    methods <- roxygen$.__S3MethodsTable__.
    replaced <- list()
    for (replacement in replacements) {
      expected_body <- deparse(replacement$original)
      if (is.null(replacement$method)) {
        name <- replacement$name
        current <- roxygen[[name]]
        s3 <- isTRUE(replacement$s3)
        if (is.function(current) && identical(deparse(body(current)), expected_body) &&
            (!s3 || identical(methods[[name]], current))) {
          environment(replacement$replacement) <- run
          unlockBinding(name, roxygen)
          assign(name, replacement$replacement, envir = roxygen)
          if (s3) assign(name, replacement$replacement, envir = methods)
          replaced[[name]] <- list(original = current, method = FALSE, s3 = s3)
        }
      } else {
        name <- replacement$method
        current <- topics$public_methods[[name]]
        if (is.function(current) && identical(deparse(body(current)), expected_body)) {
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
        if (replaced[[name]]$s3) assign(name, previous, envir = methods)
        lockBinding(name, roxygen)
      }
    }, add = TRUE)
    unchanged_meaning <- all(vapply(names(expected), function(name) {
      current <- roxygen[[name]]
      is.function(current) && identical(deparse(body(current)), deparse(expected[[name]]))
    }, logical(1L)))
    write_original <- roxygen$write_if_different
    state$incremental <- length(topic_cache) && parse_replaced && unchanged_meaning &&
      all(c("block_set_env", "roclet_process.roclet_namespace", "roclet_process.roclet_rd") %in%
            names(replaced)) &&
      identical(names(formals(write_original)), c("path", "contents", "command", "check"))
    if (state$incremental) {
      unlockBinding("write_if_different", roxygen)
      assign("write_if_different", function(path, contents, command = NULL, check = TRUE) {
        if (isTRUE(get0(basename(path), envir = state$skip, inherits = FALSE))) return(FALSE)
        write_original(path, contents, command = command, check = check)
      }, envir = roxygen)
      on.exit({
        assign("write_if_different", write_original, envir = roxygen)
        lockBinding("write_if_different", roxygen)
      }, add = TRUE)
    }
    if (nzchar(Sys.getenv("RY_DOCUMENT_DEBUG"))) {
      message("replaced: ", toString(names(replaced)), "; incremental: ", state$incremental)
    }
    roxygen2::roxygenise(root, roclets, load_code = load_code)
    if (state$incremental && !is.null(state$records)) {
      records <- state$records
      info <- file.info(file.path(root, "man", names(records)), extra_cols = FALSE)
      records <- Map(function(record, size, mtime) {
        record$size <- size
        record$mtime <- mtime
        record
      }, records, info$size, as.numeric(info$mtime))
      save_atomically(
        list(key = state$key, topics = records, blocks = as.list(state$entries, all.names = TRUE)),
        topic_cache
      )
    }
    invisible(TRUE)
  }
})
