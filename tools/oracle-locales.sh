#!/usr/bin/env bash
# Generates every locale an oracle case names in a leading `# oracle: requires locale <name>`
# line, as root. `oracle compare` refuses to run while one is missing. tools/dev-setup.sh and CI
# both run it; it skips a locale the C library can already load.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
for name in $(sed -n 's/^# oracle: requires locale //p' "$root"/crates/oracle/cases/*.R | sort -u); do
	if python3 -c 'import locale, sys; locale.setlocale(locale.LC_CTYPE, sys.argv[1])' "$name" 2>/dev/null; then
		continue
	fi
	echo "oracle-locales: generating $name"
	localedef -i "${name%%.*}" -f "${name#*.}" "$name"
done
