#!/usr/bin/env bash
# Cut a staged or final version. Bumps the version in Cargo.toml + tauri.conf.json +
# package.json, commits, tags, pushes. The release workflow builds and publishes.
#   scripts/release.sh alpha 0.11.0     -> v0.11.0-alpha.N (N = next) on 5lime-dev
#   scripts/release.sh beta  0.11.0     -> v0.11.0-beta.N  on release/0.11 (created from 5lime-dev if missing)
#   scripts/release.sh final 0.11.0     -> v0.11.0 on main (merges release/0.11 or 5lime-dev into main first)
set -euo pipefail
kind="${1:?alpha|beta|final}"; ver="${2:?X.Y.Z}"; remote="${AITERM_REMOTE:-origin}"
git fetch -q "$remote" --tags
n=$(( $(git tag -l "v$ver-$kind.*" | wc -l) + 1 ))
case "$kind" in
  alpha) branch=5lime-dev; tag="v$ver-alpha.$n"; cargo_ver="$ver-alpha.$n" ;;
  beta)  branch="release/${ver%.*}"; tag="v$ver-beta.$n"; cargo_ver="$ver-beta.$n"
         git show-ref -q "refs/remotes/$remote/$branch" || git push "$remote" "$remote/5lime-dev:refs/heads/$branch" ;;
  final) branch=main; tag="v$ver"; cargo_ver="$ver" ;;
  *) echo "kind must be alpha|beta|final"; exit 2 ;;
esac
git checkout -q "$branch" 2>/dev/null || git checkout -q -b "$branch" "$remote/$branch"
git pull -q --ff-only "$remote" "$branch"
if [ "$kind" = final ]; then
  src="release/${ver%.*}"; git show-ref -q "refs/remotes/$remote/$src" || src=5lime-dev
  git merge --no-ff "$remote/$src" -m "Release $ver: merge $src into main"
fi
sed -i "0,/^version = \".*\"/s//version = \"$cargo_ver\"/" src-tauri/Cargo.toml
sed -i "0,/\"version\": \".*\"/s//\"version\": \"$cargo_ver\"/" src-tauri/tauri.conf.json package.json
# keep Cargo.lock's own entry in step (no network needed)
sed -i "/^name = \"aiterm\"$/{n;s/^version = \".*\"/version = \"$cargo_ver\"/}" src-tauri/Cargo.lock
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/tauri.conf.json package.json
git commit -q -m "Version $cargo_ver"
git tag -a "$tag" -m "$tag"
git push "$remote" "$branch" "$tag"
echo "$tag on $branch -> $(git rev-parse --short HEAD)"
