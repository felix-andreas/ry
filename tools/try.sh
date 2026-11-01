#!/usr/bin/env bash
# `just try`: build ry and walk through what it does, in a throwaway project.
#
# Every step prints the command before its output, so the commands are what you learn. The demo
# project lives in $RY_TRY_DIR (default: a fresh temporary directory). The last step opens a
# console inside a demo package; set RY_TRY_NO_CONSOLE=1, or run without a terminal, to skip it.
set -euo pipefail

repository=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
target_dir=${CARGO_TARGET_DIR:-$repository/target}
# The demo changes directory, so a relative target directory would stop resolving.
[[ "$target_dir" = /* ]] || target_dir=$PWD/$target_dir

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
	bold=$'\e[1m' dim=$'\e[2m' cyan=$'\e[36m' red=$'\e[31m' reset=$'\e[0m'
else
	bold="" dim="" cyan="" red="" reset=""
fi

heading() { printf '\n%s== %s ==%s\n' "$bold" "$1" "$reset"; }
note() { printf '%s%s%s\n' "$dim" "$1" "$reset"; }

# Prints the command as you would type it, then runs it. A nonzero exit is part of the demo
# (`ry check` exits 1 when it finds something), so it is shown, not fatal.
show() {
	local display="" word
	for word in "$@"; do
		if [[ "$word" =~ ^[A-Za-z0-9_./:=+-]+$ ]]; then display+=" $word"; else display+=" '${word//\'/\'\\\'\'}'"; fi
	done
	printf '\n%s$%s%s\n' "$cyan" "$display" "$reset"
	local status=0
	"$@" || status=$?
	if [ "$status" -ne 0 ]; then
		note "(exit status $status)"
	fi
}

# Each missing prerequisite gets one line that says how to fix it, and all of them are reported
# at once so a fresh machine needs one round of installs, not several.
check_prerequisites() {
	local problems=()

	if ! command -v cargo >/dev/null 2>&1; then
		problems+=("Rust is not installed: install it from https://rustup.rs, then open a new shell.")
	fi
	if ! command -v cc >/dev/null 2>&1 && ! command -v gcc >/dev/null 2>&1 && ! command -v clang >/dev/null 2>&1; then
		problems+=("No C compiler found (ry's runtime has a C part): install build-essential (Debian/Ubuntu), gcc (Fedora), or run \`xcode-select --install\` (macOS).")
	fi

	local r_home=""
	if [ -n "${R_HOME:-}" ]; then
		r_home=$R_HOME
	elif command -v R >/dev/null 2>&1; then
		r_home=$(R RHOME 2>/dev/null || true)
	fi
	if [ -z "$r_home" ]; then
		problems+=("GNU R is not installed (ry's runtime reads base R from it): install it from https://cloud.r-project.org, or with \`apt install r-base-core\`, \`dnf install R-core\`, or \`brew install --cask r\`.")
	elif [ ! -e "$r_home/lib/libR.so" ] && [ ! -e "$r_home/lib/libR.dylib" ]; then
		problems+=("$r_home has no lib/libR.so or lib/libR.dylib: it is not an R installation, or that R was built without --enable-R-shlib. Install a CRAN or distribution build of R (both are shared), or fix R_HOME.")
	fi

	# A cold release build of ry and its runtime writes about 6 GB into target/.
	if [ ! -x "$target_dir/release/ry" ]; then
		mkdir -p "$target_dir"
		local free_kb
		free_kb=$(df -Pk "$target_dir" | awk 'NR == 2 { print $4 }')
		if [ -n "$free_kb" ] && [ "$free_kb" -lt $((8 * 1024 * 1024)) ]; then
			problems+=("Only $((free_kb / 1024 / 1024)) GB free under $target_dir, and the first build needs about 8 GB: free some disk space, or point CARGO_TARGET_DIR at a larger disk.")
		fi
	fi

	if [ "${#problems[@]}" -gt 0 ]; then
		printf '%serror:%s ry cannot be tried yet:\n' "$red$bold" "$reset" >&2
		for problem in "${problems[@]}"; do
			printf '  - %s\n' "$problem" >&2
		done
		exit 1
	fi
}

check_prerequisites

heading "Building ry and its runtime (fast after the first time)"
note "\$ cargo build --release -p ry-lang -p libr   (the first build takes 10 to 30 minutes)"
# Warnings from a successful build would bury the demo, so they are shown only on failure.
if ! build_log=$(cargo build --color always --release --manifest-path "$repository/Cargo.toml" -p ry-lang -p libr 2>&1); then
	printf '%s\n' "$build_log" >&2
	exit 1
fi
bin_dir="$target_dir/release"
export PATH="$bin_dir:$PATH"
ry --version

demo=${RY_TRY_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/ry-try.XXXXXX")}
# The demo is rebuilt from scratch on every run, so it deletes $RY_TRY_DIR first, but only a
# directory that is empty or that an earlier run created.
if [ -n "$(ls -A "$demo" 2>/dev/null)" ] && [ ! -e "$demo/.ry-try" ]; then
	printf '%serror:%s %s is not empty and was not made by just try; pick another RY_TRY_DIR.\n' \
		"$red$bold" "$reset" "$demo" >&2
	exit 1
fi
rm -rf "$demo"
mkdir -p "$demo/analysis" "$demo/typed" "$demo/tally/R"
touch "$demo/.ry-try"
cd "$demo"
note "The demo project is in $demo"

# --- Checking plain R ---------------------------------------------------------------------------

cat >analysis/report.R <<'EOF'
summarise_sales <- function(sales, rate) {
  total <- sum(sales$amount)
  net <- total * (1 - ratee)
  round(net, 2)
}

summarise_sales(data.frame(amount = c(10, 20)), 0.2)
EOF

heading "1. ry check finds bugs in plain R, with no setup"
show cat analysis/report.R
show ry check analysis

# --- Type checking ------------------------------------------------------------------------------

cat >typed/ry.toml <<'EOF'
[check]
typing = true
EOF
cat >typed/prices.R <<'EOF'
with_tax <- function(price: double, rate: double = 0.2) -> double {
  price * (1 + rate)
}

with_tax(100)
with_tax("100")
EOF

heading "2. Turn on typing, and ry checks types too"
note "Annotations are written inline (as here) or in #: comments, which keep the file plain R."
show cat typed/ry.toml typed/prices.R
show ry check typed

# --- Formatting ---------------------------------------------------------------------------------

cat >typed/messy.R <<'EOF'
scale_to<-function(x,lo=0,hi=1){
rng<-range(x,na.rm=TRUE)
(x-rng[1])/(rng[2]-rng[1])*(hi-lo)+lo}
EOF

heading "3. ry fmt formats code"
show ry fmt --diff typed/messy.R
show ry fmt typed/messy.R
show cat typed/messy.R

# --- Running ------------------------------------------------------------------------------------

cat >hello.R <<'EOF'
fib <- function(n) if (n < 2) n else fib(n - 1) + fib(n - 2)
cat("fib(20) =", fib(20), "\n")
print(summary(c(3, 1, 4, 1, 5, 9, 2, 6)))
EOF

heading "4. ry run runs a script on ry's runtime; ry R runs it on GNU R"
show cat hello.R
show ry run hello.R
show ry R hello.R

# --- Records, tagged unions and match -----------------------------------------------------------

cat >shapes.R <<'EOF'
alias Shape = :Circle(radius: double) | :Rect(width: double, height: double) | :Empty

area <- function(s: Shape) -> double {
  match (s) {
    :Circle(radius = r) => pi * r^2
    :Rect(width = w, height = h) => w * h
    :Empty => 0
  }
}

origin <- (x = 0, y = 0)
cat("origin is at", origin$x, origin$y, "\n")
cat("circle:", area(:Circle(radius = 1)), "\n")
cat("rect:  ", area(:Rect(width = 2, height = 3)), "\n")
EOF

heading "5. ry's own records, tagged unions and match"
note "match is checked for exhaustiveness: delete the :Empty arm and ry check names the missing case."
show cat shapes.R
show ry run shapes.R

# --- DataFrame ----------------------------------------------------------------------------------

cat >sales.R <<'EOF'
sales <- DataFrame(
  region = c("eu", "us", "eu", "asia", "us"),
  units = c(3L, 5L, 2L, 7L, 1L),
  price = c(1.5, 2, 3, 0.5, 4)
)
sales |>
  DataFrame.filter(col("units") > 1L) |>
  DataFrame.with_columns(revenue = col("units") * col("price")) |>
  DataFrame.group_by("region") |>
  GroupBy.agg(col("revenue") |> Expr.sum(), orders = Expr.len()) |>
  DataFrame.sort("revenue", descending = TRUE)
EOF

heading "6. DataFrame, ry's built-in typed table, runs on the Polars engine"
show cat sales.R
show ry run sales.R

# --- Package development ------------------------------------------------------------------------

cat >tally/DESCRIPTION <<'EOF'
Package: tally
Version: 0.1.0
Title: Count Words
Description: A demo package for trying ry.
License: MIT
Encoding: UTF-8
EOF
cat >tally/NAMESPACE <<'EOF'
export(count_words)
EOF
cat >tally/R/count.R <<'EOF'
count_words <- function(text) {
  length(strsplit(text, " ")[[1]])
}
EOF

heading "7. ry load loads a package from source, lazily"
show cat tally/R/count.R
cd tally
show ry load -e 'count_words("the quick brown fox")'

if [ -n "${RY_TRY_NO_CONSOLE:-}" ] || [ ! -t 0 ] || [ ! -t 1 ]; then
	heading "Done"
	note "Skipped the console (no terminal, or RY_TRY_NO_CONSOLE is set)."
	note "Try it yourself: cd $demo/tally && $bin_dir/ry R"
	exit 0
fi

heading "8. The console reloads your edits before every command"
cat <<EOF
Opening \`ry R\` inside the package. Try:

  > count_words("one two three")
  then edit $demo/tally/R/count.R in another window
  (for example, make it return the words) and run the same command again.

\`ry run\` with no file opens the same console on ry's runtime, and it reloads too.
Quit with q() or Ctrl-D. To use ry from your shell later, add it to your PATH:

  export PATH="$bin_dir:\$PATH"

EOF
show ry R
