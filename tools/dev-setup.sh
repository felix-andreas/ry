#!/usr/bin/env bash
# Provisions an Ubuntu 24.04 container (a cloud agent session or a fresh dev box) for ry
# development, as root: the system libraries ry, GNU R and the corpus packages link, a `just` new
# enough to parse the justfile, and the oracle's reference GNU R with the packages its cases need.
# It is idempotent, so the cloud environment's setup script runs it at every fresh container.
# `just doctor` checks the result.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
r_version=$(cat "$root/tools/base-r/VERSION")
r_prefix=/opt/R/$r_version
# 1.40 is the oldest release that parses the justfile; tools/doctor.py checks the same floor.
just_version=1.59.0
jobs=$(nproc)

log() { printf '[dev-setup %s] %s\n' "$(date -u +%H:%M:%S)" "$*"; }

# R's build (gfortran, X11 and cairo through pango, ICU, LAPACK 3.12), the corpus packages'
# libraries (tools/corpus/system-packages.txt plus libuv for fs, ragg's font stack), ry's own link
# (zlib, bzip2, xz) and the measuring tools agents reach for (valgrind, GNU time).
apt_packages=(
	build-essential gfortran libreadline-dev libx11-dev libxt-dev libpcre2-dev
	libcurl4-openssl-dev libbz2-dev liblzma-dev zlib1g-dev libzstd-dev libdeflate-dev libicu-dev
	libpango1.0-dev libcairo2-dev libjpeg-dev libpng-dev libtiff-dev liblapack-dev
	libssl-dev libxml2-dev libgit2-dev unixodbc-dev libpoppler-cpp-dev libglpk-dev libnlopt-dev
	libsodium-dev libuv1-dev libfreetype-dev libharfbuzz-dev libfribidi-dev libwebp-dev
	libfontconfig1-dev
	valgrind time curl ca-certificates python3-pip
)
missing=()
for package in "${apt_packages[@]}"; do
	dpkg-query -W -f='${Status}' "$package" 2>/dev/null | grep -q "install ok installed" || missing+=("$package")
done
if [ ${#missing[@]} -gt 0 ]; then
	log "apt: installing ${#missing[@]} packages"
	apt-get update -qq
	DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends "${missing[@]}" >/dev/null
fi

if [ "$(just --version 2>/dev/null)" != "just $just_version" ]; then
	log "just: installing $just_version"
	python3 -m pip install --quiet --root-user-action=ignore --break-system-packages "rust-just==$just_version"
fi

# The reference build of crates/oracle/cases/gnu-r.txt, configured as
# docs/src/content/docs/development/corpus-and-oracle.md ("The reference GNU R") says. Memory
# profiling and cairo are what other builds most often lack: without them the Rprofmem and
# graphics cases differ from their recordings. The shared libR.so is what ry's console loads for
# GNU R, and what Posit's binary packages link.
if ! "$r_prefix/bin/Rscript" -e 'stopifnot(capabilities("profmem"), capabilities("cairo"))' >/dev/null 2>&1 ||
	[ ! -f "$r_prefix/lib/R/lib/libR.so" ]; then
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
ln -sf "$r_prefix/bin/R" /usr/local/bin/R
ln -sf "$r_prefix/bin/Rscript" /usr/local/bin/Rscript

# The packages the oracle's `# oracle: requires` lines name, into the reference R's own library.
# They come as Ubuntu binaries from Posit's CRAN mirror, which serves a binary only to a user agent
# that starts with `R/<version>`: the set installs in seconds instead of five minutes. rlang is
# pinned to the release ry's native trace_back follows (FAST_PATHS in
# crates/interp/src/package_fast_paths.rs; CI pins the same) and builds from source.
log "R packages: checking the oracle's packages"
"$r_prefix/bin/Rscript" - "$jobs" <<'EOF'
jobs <- as.integer(commandArgs(TRUE)[[1]])
version <- paste(getRversion(), R.version$platform, R.version$arch, R.version$os)
options(
  HTTPUserAgent = sprintf("R/%s R (%s)", getRversion(), version),
  repos = c(CRAN = "https://packagemanager.posit.co/cran/__linux__/noble/latest"),
  Ncpus = jobs
)
wanted <- c("data.table", "bit64", "knitr", "rmarkdown", "ragg", "lattice", "R6", "withr", "xts", "testthat")
missing <- setdiff(wanted, rownames(installed.packages()))
if (length(missing)) install.packages(missing, quiet = TRUE)
rlang <- "1.3.0"
if (!requireNamespace("rlang", quietly = TRUE) || format(packageVersion("rlang")) != rlang) {
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
