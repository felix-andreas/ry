#!/usr/bin/env bash
# Provisions an Ubuntu 24.04 container (a cloud agent session or a fresh dev box) for ry
# development, as root: the system libraries ry, GNU R and the corpus packages link, a `just` new
# enough to parse the justfile, and the oracle's reference GNU R with the packages its cases need.
# It is idempotent, so a cloud environment can run it as its setup script in every fresh container.
# `just doctor` checks the result.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
# 1.40 is the oldest release that parses the justfile; tools/doctor.py checks the same floor.
just_version=1.59.0

log() { printf '[dev-setup %s] %s\n' "$(date -u +%H:%M:%S)" "$*"; }

# R's build (gfortran, X11 and cairo through pango, ICU, LAPACK 3.12), the corpus packages'
# libraries (tools/corpus/system-packages.txt plus libuv for fs, ragg's font stack), ry's own link
# (zlib, bzip2, xz), the measuring tools agents reach for (valgrind, GNU time) and sccache.
apt_packages=(
	build-essential gfortran libreadline-dev libx11-dev libxt-dev libpcre2-dev
	libcurl4-openssl-dev libbz2-dev liblzma-dev zlib1g-dev libzstd-dev libdeflate-dev libicu-dev
	libpango1.0-dev libcairo2-dev libjpeg-dev libpng-dev libtiff-dev liblapack-dev
	libssl-dev libxml2-dev libgit2-dev unixodbc-dev libpoppler-cpp-dev libglpk-dev libnlopt-dev
	libsodium-dev libuv1-dev libfreetype-dev libharfbuzz-dev libfribidi-dev libwebp-dev
	libfontconfig1-dev
	valgrind time curl ca-certificates python3-pip zstd sccache
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

# sccache for the invoking user's cargo, so a fresh worktree's target directory compiles its
# dependencies from the shared cache: a cold debug build of ry takes about 1 minute instead of 4.5.
# Edited workspace crates compile incrementally, which sccache passes through untouched.
cargo_config=${CARGO_HOME:-$(getent passwd "${SUDO_USER:-root}" | cut -d: -f6)/.cargo}/config.toml
if ! grep -q "rustc-wrapper" "$cargo_config" 2>/dev/null; then
	if grep -q "^\[build\]" "$cargo_config" 2>/dev/null; then
		log "sccache: $cargo_config has a [build] table; add rustc-wrapper = \"sccache\" to it"
	else
		log "sccache: wrapping rustc in $cargo_config"
		mkdir -p "$(dirname "$cargo_config")"
		printf '\n[build]\nrustc-wrapper = "sccache"\n' >>"$cargo_config"
		chown "${SUDO_USER:-root}" "$(dirname "$cargo_config")" "$cargo_config"
	fi
fi

# The oracle's reference GNU R and its packages, restored from the binary cache when it holds this
# exact build (tools/cache.sh) and built otherwise.
bash "$root/tools/reference-r.sh"

# The package-test corpus only when the cache has it: installing from source takes minutes, so that
# stays an explicit `just bench-packages install`.
if [ ! -d "${RY_CORPUS_DIR:-$root/.corpus}/packages/library" ]; then
	bash "$root/tools/cache.sh" restore corpus || log "corpus: not cached; run just bench-packages install when you need it"
fi
log "done"
