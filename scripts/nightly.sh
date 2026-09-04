#!/usr/bin/env bash
# End of day: freeze today's 5lime-dev as tonight's nightly.
#   scripts/nightly.sh            -> tag nightly-<today> on 5lime-dev and push it
#   scripts/nightly.sh 20260903   -> backdate (tags the current 5lime-dev tip with that date)
# The release workflow builds and publishes the prerelease from the tag.
set -euo pipefail
day="${1:-$(TZ=America/New_York date +%Y%m%d)}"
remote="${AITERM_REMOTE:-origin}"
git fetch -q "$remote" 5lime-dev --tags
tip=$(git rev-parse "$remote/5lime-dev")
if git rev-parse -q --verify "refs/tags/nightly-$day" >/dev/null; then
  echo "nightly-$day already exists at $(git rev-parse --short nightly-$day)"; exit 1; fi
git tag -a "nightly-$day" "$tip" -m "Nightly $day"
git push "$remote" "nightly-$day"
echo "nightly-$day -> $(git rev-parse --short "$tip")  (release workflow will publish it)"
