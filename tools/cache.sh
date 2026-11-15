#!/usr/bin/env bash
# Prebuilt trees in the public binary cache ry.cachix.org, so that a fresh container restores in
# seconds what takes minutes to build:
#
#   r        the oracle's reference GNU R with its packages (/opt/R/<version>, tools/reference-r.sh)
#   corpus   the package-test corpus library (.corpus/packages/library, `just bench-packages install`)
#
# Each tree travels as a zstd tarball that `nix store add-file` turns into a store path, so nix
# fetches and verifies it like any other path. tools/cache.lock maps a hash of everything the tree
# was built from (the scripts, the pinned versions, the Ubuntu release) to that store path and its
# size, and a tree is restored only when its hash is in the lock. Restoring needs nix but no token.
# Pushing needs the cache's write token, which a cloud session's proxy injects for app.cachix.org.
# Anyone can read the cache, so only trees built from public sources (CRAN, GNU R) go into it.
#
#   tools/cache.sh restore r|corpus   restore the tree if the lock has its hash, else exit 1
#   tools/cache.sh push r|corpus      check the tree, pack it, push it, record it in tools/cache.lock
#   tools/cache.sh hash r|corpus      print the inputs hash, which `just doctor` looks up in the lock
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
lock=$root/tools/cache.lock
cache=ry
substituter=https://ry.cachix.org
public_key=ry.cachix.org-1:6OKbqSFUFIsagxwxTo06JWHSEWuEtQp4kgQtR/TuVyY=
# The cache holds 5 GB. The trees the lock names may fill half of it, so a superseded copy can stay
# until Cachix evicts the least recently used paths.
budget_bytes=$((2500 * 1024 * 1024))
r_version=$(cat "$root/tools/base-r/VERSION")
# sudo's secure_path drops the nix profile.
PATH=$PATH:/nix/var/nix/profiles/default/bin

log() { printf '[cache %s] %s\n' "$(date -u +%H:%M:%S)" "$*" >&2; }

inputs_hash() {
	case $1 in
	r)
		cat "$root/tools/reference-r.sh" "$root/tools/base-r/VERSION" "$root/tools/base-r/SHA256SUMS"
		if [ -r /etc/os-release ]; then
			(. /etc/os-release && echo "$ID $VERSION_ID $(uname -m)")
		else
			echo "$(uname -s) $(uname -m)"
		fi
		;;
	corpus)
		inputs_hash r
		cat "$root/tools/bench/packages/lock.tsv"
		;;
	esac | sha256sum | cut -c1-32
}

directory() {
	case $1 in
	r) echo "/opt/R/$r_version" ;;
	corpus) echo "${RY_CORPUS_DIR:-$root/.corpus}/packages/library" ;;
	esac
}

usage() {
	echo "usage: tools/cache.sh restore|push|hash r|corpus" >&2
	exit 2
}

command=${1:-}
tree=${2:-}
case $tree in
r | corpus) ;;
*) usage ;;
esac
hash=$(inputs_hash "$tree")
target=$(directory "$tree")

# The tarballs are zstd. Without it on PATH, take it from nixpkgs, which restoring needs anyway,
# rather than from apt: this script also runs on its own (tools/reference-r.sh calls it), without
# root, and while an Ubuntu mirror is down.
if [ "$command" != hash ] && ! command -v zstd >/dev/null && command -v nix >/dev/null; then
	log "zstd: not on PATH, so taking it from nixpkgs"
	zstd_bin=$(nix build --option warn-dirty false --no-link --print-out-paths --inputs-from "$root" 'nixpkgs#zstd^bin') || {
		log "zstd: nix could not provide it; run sudo apt-get install zstd"
		exit 1
	}
	PATH=$zstd_bin/bin:$PATH
fi

case $command in
hash) echo "$hash" ;;
restore)
	path=$(awk -v tree="$tree" -v hash="$hash" '$1 == tree && $2 == hash { print $3 }' "$lock" 2>/dev/null)
	if [ -z "$path" ]; then
		log "$tree: the cache has no build of these inputs ($hash)"
		exit 1
	fi
	if ! command -v nix-store >/dev/null; then
		log "$tree: nix is not installed, so the cache is out of reach"
		exit 1
	fi
	if ! errors=$(nix-store --realise "$path" --option extra-substituters "$substituter" \
		--option extra-trusted-public-keys "$public_key" 2>&1 >/dev/null); then
		log "$tree: fetching $path from $substituter failed: $(tail -n 3 <<<"$errors")"
		exit 1
	fi
	# Extract beside the target and swap it in, so a failed or interrupted restore never leaves a
	# partial tree that later runs would take for a complete one.
	mkdir -p "$(dirname "$target")"
	staging=$(mktemp -d "$(dirname "$target")/.restore.XXXXXX")
	trap 'rm -rf "$staging"' EXIT
	tar --zstd --no-same-owner -xf "$path" -C "$staging"
	# The corpus lives in the user's checkout, which `sudo tools/dev-setup.sh` must not leave
	# owned by root.
	if [ "$tree" = corpus ] && [ "$(id -u)" = 0 ]; then
		chown -R --reference="$root" "$staging" "${RY_CORPUS_DIR:-$root/.corpus}"
	fi
	rm -rf "${target:?}"
	mv "$staging/$(basename "$target")" "$target"
	# The tarball has served its purpose; keeping it would hold its size on disk twice.
	nix-store --delete "$path" >/dev/null 2>&1 || log "$tree: $path stays in the nix store until the next garbage collection"
	log "$tree: restored $target"
	;;
push)
	# Push only a tree that matches the inputs its hash names: a stale tree recorded under a new
	# hash would reach every container without a warning.
	case $tree in
	r)
		bash "$root/tools/reference-r.sh" check || {
			echo "error: $target is not the reference R that tools/reference-r.sh builds; run sudo tools/dev-setup.sh first" >&2
			exit 1
		}
		;;
	corpus)
		stale=$(awk -F '\t' -v library="$target" '!/^#/ && NF >= 2 {
			version = ""
			file = library "/" $1 "/DESCRIPTION"
			while ((getline line < file) > 0) if (line ~ /^Version: /) version = substr(line, 10)
			close(file)
			if (version != $2) print $1 " " $2
		}' "$root/tools/bench/packages/lock.tsv")
		if [ -n "$stale" ]; then
			echo "error: $target does not hold these locked versions; run just bench-packages install first:" >&2
			echo "$stale" | head -5 >&2
			exit 1
		fi
		;;
	esac
	work=$(mktemp -d)
	trap 'rm -rf "$work"' EXIT
	tarball=$work/ry-$tree-$hash.tar.zst
	tar --zstd -cf "$tarball" -C "$(dirname "$target")" "$(basename "$target")"
	size=$(stat -c %s "$tarball")
	others=$(awk -v tree="$tree" '$1 != tree { sum += $4 } END { print sum + 0 }' "$lock" 2>/dev/null || echo 0)
	if [ $((others + size)) -gt "$budget_bytes" ]; then
		echo "error: the trees in tools/cache.lock would take $(((others + size) >> 20)) MiB, over the $((budget_bytes >> 20)) MiB budget" >&2
		exit 1
	fi
	path=$(nix store add-file "$tarball")
	rm -rf "$work"
	log "$tree: pushing $path ($((size >> 20)) MiB)"
	# cachix posts to cachix.org unless told otherwise, and the proxy injects the token only for
	# app.cachix.org. Outside a cloud session, set CACHIX_AUTH_TOKEN to the real token.
	CACHIX_AUTH_TOKEN=${CACHIX_AUTH_TOKEN:-proxy-injected} nix shell --inputs-from "$root" nixpkgs#cachix \
		-c cachix --hostname https://app.cachix.org push "$cache" "$path"
	nix-store --delete "$path" >/dev/null 2>&1 || log "$tree: $path stays in the nix store until the next garbage collection"
	{
		grep -v "^$tree " "$lock" 2>/dev/null || true
		echo "$tree $hash $path $size"
	} | sort >"$lock.new"
	mv "$lock.new" "$lock"
	log "$tree: recorded in tools/cache.lock; commit it"
	;;
*) usage ;;
esac
