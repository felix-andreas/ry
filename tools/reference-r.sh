#!/usr/bin/env bash
# Installs the oracle's reference GNU R into /opt/R/<version>, with the packages its cases need, as
# root; tools/dev-setup.sh runs it after the system libraries are in place. tools/cache.sh keys its
# prebuilt copy on this file's text, so any edit here makes the next container build R from source
# until someone runs `just cache-push r`.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
r_version=$(cat "$root/tools/base-r/VERSION")
r_prefix=/opt/R/$r_version
jobs=$(nproc)

log() { printf '[reference-r %s] %s\n' "$(date -u +%H:%M:%S)" "$*"; }

# Memory profiling, ICU and cairo bitmaps are what other builds most often lack: without them the
# Rprofmem, collation and graphics cases differ from their recordings. capabilities("cairo") is not
# enough: a build without pango-cairo reports it, but its png() falls back to bitmapType "Xlib",
# which fails headless with "X11 is not available", hence the check without DISPLAY. The shared
# libR.so is what ry's console loads for GNU R, and what Posit's binary packages link.
capable() {
	env -u DISPLAY "$r_prefix/bin/Rscript" -e \
		'stopifnot(capabilities("profmem"), capabilities("ICU"), getOption("bitmapType") == "cairo")' >/dev/null 2>&1 &&
		[ -f "$r_prefix/lib/R/lib/libR.so" ]
}

# `tools/reference-r.sh check` installs nothing and fails unless the R in $r_prefix and its packages
# are what this script would produce; tools/cache.sh runs it before pushing the tree.
mode=${1:-install}

if [ "$mode" = install ] && ! capable && bash "$root/tools/cache.sh" restore r; then
	log "GNU R $r_version: restored from the binary cache"
fi

# The reference build of crates/oracle/cases/gnu-r.txt, configured as
# docs/src/content/docs/development/corpus-and-oracle.md ("The reference GNU R") says. A restored
# tree that fails the check is rebuilt here too.
if ! capable; then
	if [ "$mode" = check ]; then
		log "GNU R $r_version: $r_prefix lacks memory profiling, ICU, cairo bitmaps or libR.so"
		exit 1
	fi
	log "GNU R $r_version: building into $r_prefix"
	source_dir=$(mktemp -d)
	tarball="$source_dir/R-$r_version.tar.gz"
	curl -fsSL --retry 5 --retry-all-errors -o "$tarball" "https://cloud.r-project.org/src/base/R-${r_version%%.*}/R-$r_version.tar.gz"
	(cd "$source_dir" && grep " R-$r_version.tar.gz\$" "$root/tools/base-r/SHA256SUMS" | sha256sum -c --quiet)
	tar -xzf "$tarball" -C "$source_dir"
	(
		cd "$source_dir/R-$r_version"
		./configure --prefix="$r_prefix" --enable-R-shlib --enable-memory-profiling --with-x \
			--without-recommended-packages --disable-java >"$source_dir/configure.log" 2>&1
		make -j"$jobs" >"$source_dir/make.log" 2>&1
		make install >/dev/null 2>&1
	) || {
		log "GNU R build failed; the logs are in $source_dir"
		exit 1
	}
	rm -rf "$source_dir"
fi
if [ "$mode" = install ]; then
	ln -sf "$r_prefix/bin/R" /usr/local/bin/R
	ln -sf "$r_prefix/bin/Rscript" /usr/local/bin/Rscript
fi

# The packages the oracle's `# oracle: requires` lines name, into the reference R's own library.
# They come as Ubuntu binaries from Posit's CRAN mirror, which serves a binary only to a user agent
# that starts with `R/<version>`: the set installs in seconds instead of five minutes. rlang is
# pinned to the release ry's native trace_back follows (FAST_PATHS in
# crates/interp/src/package_fast_paths.rs; CI pins the same) and builds from source.
log "R packages: checking the oracle's packages"
"$r_prefix/bin/Rscript" - "$jobs" "$mode" <<'EOF'
jobs <- as.integer(commandArgs(TRUE)[[1]])
check <- commandArgs(TRUE)[[2]] == "check"
version <- paste(getRversion(), R.version$platform, R.version$arch, R.version$os)
options(
  HTTPUserAgent = sprintf("R/%s R (%s)", getRversion(), version),
  repos = c(CRAN = "https://packagemanager.posit.co/cran/__linux__/noble/latest"),
  Ncpus = jobs
)
wanted <- c("data.table", "bit64", "knitr", "rmarkdown", "ragg", "lattice", "R6", "withr", "xts", "testthat")
missing <- setdiff(wanted, rownames(installed.packages()))
rlang <- "1.3.0"
pinned <- requireNamespace("rlang", quietly = TRUE) && format(packageVersion("rlang")) == rlang
if (check && (length(missing) || !pinned)) stop("missing: ", paste(c(missing, if (!pinned) paste("rlang", rlang)), collapse = ", "))
if (length(missing)) install.packages(missing, quiet = TRUE)
if (!pinned) {
  urls <- sprintf(c("https://cloud.r-project.org/src/contrib/rlang_%s.tar.gz",
                    "https://cloud.r-project.org/src/contrib/Archive/rlang/rlang_%s.tar.gz"), rlang)
  for (url in urls) {
    try(install.packages(url, repos = NULL, type = "source", quiet = TRUE))
    if (requireNamespace("rlang", quietly = TRUE) && format(packageVersion("rlang")) == rlang) break
  }
}
still <- setdiff(c(wanted, "rlang"), rownames(installed.packages()))
if (length(still)) stop("could not install: ", paste(still, collapse = ", "))
EOF
log "done"
