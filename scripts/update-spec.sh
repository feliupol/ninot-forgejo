#!/bin/sh
# Pins spec/forgejo.v1.json to Forgejo's Swagger description at a release tag.
# Usage: scripts/update-spec.sh v16.0.3
set -eu
cd "$(dirname "$0")/.."
tag=$1
url="https://codeberg.org/forgejo/forgejo/raw/tag/$tag/templates/swagger/v1_json.tmpl"
# The template's only placeholders are the sub-URL (empty at the root) and the version.
curl -fsSL "$url" \
    | sed -e 's/{{AppSubUrl | JSEscape}}//' -e "s/{{AppVer | JSEscape}}/${tag#v}/" \
    > spec/forgejo.v1.json
printf '%s\n' "$url" > spec/SOURCE
echo "pinned forgejo $tag"
