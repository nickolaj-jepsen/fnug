#!/bin/sh
# Prints CHANGELOG.md's entries for a version, for its GitHub release; fails when it has none.
set -eu
version=$1
changelog=${2:-CHANGELOG.md}
notes=$(awk -v heading="## [$version] - " '
  index($0, "## [") == 1 { if (found) exit; found = index($0, heading) == 1; next }
  /^\[[^]]+\]: / { if (found) exit }
  found
' "$changelog" | sed '/./,$!d')
if [ -z "$notes" ]; then
  echo "error: $changelog has no entries under '## [$version] - YYYY-MM-DD'" >&2
  exit 1
fi
printf '%s\n' "$notes"
