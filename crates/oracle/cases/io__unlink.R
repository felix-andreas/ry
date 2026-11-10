# unlink() follows GNU R's R_unlink: a missing path is not a failure, an NA path is, a
# directory needs recursive = TRUE (even an empty one), and a symbolic link is removed as a
# link, never followed. The path is gone as soon as unlink() returns.
root <- tempfile("unlink")
dir.create(root)
at <- function(...) file.path(root, ...)
touch <- function(...) invisible(file.create(at(...)))
tree <- function() sort(list.files(root, recursive = TRUE, all.files = TRUE, include.dirs = TRUE))

touch("a.txt")
print(unlink(at("a.txt")))
print(file.exists(at("a.txt")))
print(unlink(at("missing")))
print(unlink(character()))
print(unlink(NA_character_))
print(unlink(c(at("missing"), NA)))
print(unlink("~"))

# Without recursive, a directory stays and the call fails.
dir.create(at("empty"))
print(unlink(at("empty")))
print(dir.exists(at("empty")))
print(unlink(at("empty"), recursive = TRUE))
print(dir.exists(at("empty")))

# A nested tree with hidden entries, removed and made again under the same name.
for (round in 1:3) {
  dir.create(at("t", "b", "c"), recursive = TRUE)
  touch(c("t/x", "t/.hidden", "t/b/y", "t/b/c/z"))
  dir.create(at("t", "b", "d"))
  print(unlink(at("t"), recursive = TRUE))
  print(c(dir.exists(at("t")), file.exists(at("t", "x"))))
}
dir.create(at("slash"))
touch("slash/f")
print(unlink(paste0(at("slash"), "/"), recursive = TRUE))
print(dir.exists(at("slash")))

# recursive = TRUE on a plain file removes it.
touch("plain")
print(unlink(at("plain"), recursive = TRUE))
print(file.exists(at("plain")))

# Symbolic links are removed, and what they point to is untouched.
dir.create(at("target"))
touch("target/keep")
file.symlink(at("target"), at("link"))
print(unlink(at("link"), recursive = TRUE))
print(c(file.exists(at("link")), file.exists(at("target", "keep"))))
dir.create(at("holder"))
file.symlink(at("target"), at("holder", "inner"))
file.symlink(at("nowhere"), at("holder", "dangling"))
print(unlink(at("holder"), recursive = TRUE))
print(c(dir.exists(at("holder")), file.exists(at("target", "keep"))))
file.symlink(at("nowhere"), at("dangling"))
print(unlink(at("dangling")))
print(at("dangling") %in% list.files(root, full.names = TRUE))
print(tree())

# With expand = TRUE (the default) each path is a glob pattern; a pattern that matches nothing
# names a file literally.
touch(c("g1.txt", "g2.txt", "g.csv", "[x]", "x", "star*"))
print(unlink(at("*.txt")))
print(tree())
print(unlink(at("[x]")))
print(tree())
print(unlink(at("[x]")))
print(tree())
print(unlink(at("nothing*")))
print(unlink(at("star*"), expand = FALSE))
print(tree())
print(unlink(at("*"), recursive = TRUE))
print(tree())

# force = TRUE makes read-only directories and files writable first.
dir.create(at("locked", "deep"), recursive = TRUE)
touch(c("locked/f", "locked/deep/g"))
Sys.chmod(at("locked", "deep", "g"), "0444")
Sys.chmod(at("locked", "deep"), "0555")
Sys.chmod(at("locked"), "0555")
print(unlink(at("locked"), recursive = TRUE, force = TRUE))
print(dir.exists(at("locked")))

# Removing the tree that holds the working directory.
dir.create(at("cwd", "inside"), recursive = TRUE)
touch("cwd/f")
home <- setwd(at("cwd", "inside"))
print(unlink(at("cwd"), recursive = TRUE))
setwd(home)
print(dir.exists(at("cwd")))

# Arguments are checked as GNU R checks them.
print(tryCatch(unlink(1), error = conditionMessage))
print(tryCatch(unlink(root, recursive = NA), error = conditionMessage))
print(tryCatch(unlink(root, force = NA), error = conditionMessage))
print(tryCatch(unlink(root, expand = NA), error = conditionMessage))
print(unlink(root, recursive = TRUE))
print(dir.exists(root))
