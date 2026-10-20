#!/usr/bin/env bash
# Decides whether a pushed commit is a release. Its first line must be exactly
# "Release vX.Y.Z" (or "Release vX.Y.Z-beta.1" for a pre-release), and X.Y.Z must match
# the version in Cargo.toml so `mcsa --version` agrees with the release.
#
# Reads the commit message from $MESSAGE and writes release/version/prerelease to $GITHUB_OUTPUT.
set -euo pipefail

out=${GITHUB_OUTPUT:-/dev/stdout}
subject=$(printf '%s\n' "$MESSAGE" | head -n1)
cargo_version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n1)

if [[ ! $subject =~ ^Release\ v([0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?)$ ]]; then
    echo "::notice::\"$subject\" isn't a release commit (expected \"Release vX.Y.Z\"), nothing to do"
    echo "release=false" >>"$out"
    exit 0
fi
version=${BASH_REMATCH[1]}

if [[ $version != "$cargo_version" ]]; then
    echo "::error file=Cargo.toml::The commit says v$version but Cargo.toml says $cargo_version. Set version = \"$version\" in Cargo.toml, run cargo build to update Cargo.lock, and commit both as \"Release v$version\"."
    exit 1
fi

if git ls-remote --exit-code --tags origin "refs/tags/v$version" >/dev/null 2>&1; then
    echo "::error::v$version has already been released. Bump the version in Cargo.toml."
    exit 1
fi

prerelease=false
[[ $version == *-* ]] && prerelease=true
{
    echo "release=true"
    echo "version=$version"
    echo "prerelease=$prerelease"
} >>"$out"
echo "Releasing v$version"
