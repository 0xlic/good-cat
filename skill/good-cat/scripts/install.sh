#!/bin/bash
# macOS release bootstrap. No downloaded code is run by this script.
set -euo pipefail
umask 077

fail() { printf 'good-cat: %s\n' "$*" >&2; exit 1; }
repo= version=latest cache= force=0 stage= payload_stage=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --repo|--version|--cache-dir)
            [ "$#" -ge 2 ] && [ -n "$2" ] || fail "missing value for $1"
            case "$1" in
                --repo) repo=$2 ;; --version) version=$2 ;; --cache-dir) cache=$2 ;;
            esac
            shift 2 ;;
        --force) force=1; shift ;;
        *) fail "unknown argument: $1" ;;
    esac
done
[[ "$repo" =~ ^[A-Za-z0-9][A-Za-z0-9-]*/[A-Za-z0-9][A-Za-z0-9._-]*$ ]] || fail '--repo OWNER/REPO is required'
[[ "$repo" != *..* ]] || fail 'invalid repository'
valid_tag() { [[ "$1" =~ ^[A-Za-z0-9][A-Za-z0-9._+-]*$ ]] && [[ "$1" != *..* ]]; }
valid_tag "$version" || fail 'version must be a safe, single filename tag'
[ "$(uname -s)" = Darwin ] || fail 'unsupported host: only macOS is supported by install.sh'
# uname can report x86_64 in a Rosetta shell; prefer the native hardware.
if [ "$(sysctl -n hw.optional.arm64 2>/dev/null || :)" = 1 ]; then
    target=aarch64-apple-darwin
else
    case "$(uname -m)" in
        arm64|aarch64) target=aarch64-apple-darwin ;;
        x86_64|amd64) target=x86_64-apple-darwin ;;
        *) fail 'unsupported macOS architecture' ;;
    esac
fi
asset=good-cat-$target
for tool in curl plutil shasum mktemp; do command -v "$tool" >/dev/null || fail "required system tool missing: $tool"; done
agent_dir=$(pwd -P)
if [ -z "$cache" ]; then
    cache=$agent_dir/.good-cat/bin
fi
case "$cache" in /*) ;; *) cache=$agent_dir/$cache ;; esac
# Resolve existing ancestors before checking containment; disallow ambiguous traversal.
case "/$cache/" in */../*|*/./*) fail 'cache path must not contain . or .. components' ;; esac
canonical_dir() {
    local path=$1 suffix= name
    while [ ! -d "$path" ]; do
        [ ! -e "$path" ] && [ ! -L "$path" ] || return 1
        name=${path##*/}; suffix=/$name$suffix
        path=$(dirname "$path")
    done
    printf '%s%s\n' "$(cd "$path" && pwd -P)" "$suffix"
}
cache=$(canonical_dir "$cache") || fail 'invalid cache directory'
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
skill_dir=$(cd "$script_dir/.." && pwd -P)
# Everything must stay inside the Agent's current directory for easy cleanup.
case "$cache/" in "$agent_dir/"*) ;; *) fail "cache must be inside the Agent directory: $agent_dir" ;; esac
case "$cache/" in "$skill_dir/"*) fail 'cache must be outside the skill directory' ;; esac
marker=$cache/.good-cat-release-cache
if [ -e "$cache" ]; then
    [ -d "$cache" ] && [ ! -L "$cache" ] || fail 'cache path is not a real directory'
    [ "$(stat -f '%u' "$cache")" = "$(id -u)" ] || fail 'cache is owned by another user'
    [ "$(( 8#$(stat -f '%Lp' "$cache") & 077 ))" -eq 0 ] || fail 'existing cache permissions are too broad'
    if [ ! -e "$marker" ]; then
        [ -z "$(ls -A "$cache")" ] || fail 'existing cache is not owned by good-cat; choose another cache path'
        printf 'good-cat-release-cache-v1\n' > "$marker"
    fi
    [ -f "$marker" ] && [ ! -L "$marker" ] || fail 'existing cache marker is invalid'
    [ "$(< "$marker")" = good-cat-release-cache-v1 ] || fail 'invalid cache marker'
else
    mkdir -p "$(dirname "$cache")"
    mkdir -m 700 "$cache"
    printf 'good-cat-release-cache-v1\n' > "$marker"
fi
stage=$(mktemp -d "$cache/.stage.XXXXXXXX")
cleanup() {
    [ -z "$payload_stage" ] || rm -rf "$payload_stage"
    [ -z "$stage" ] || rm -rf "$stage"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
fetch() {
    curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
        --connect-timeout 15 --max-time 180 \
        --header 'Accept: application/vnd.github+json' --header 'User-Agent: good-cat-bootstrap' \
        --output "$2" "$1" || fail "download failed: $1"
}
# Tags are restricted to ASCII filename characters; only '+' needs URL encoding.
encode_tag() { printf '%s' "${1//+/%2B}"; }
api=https://api.github.com/repos/$repo/releases
requested_version=$version
if [ "$version" = latest ]; then
    fetch "$api/latest" "$stage/release.json"
else
    fetch "$api/tags/$(encode_tag "$version")" "$stage/release.json"
fi
json_field() { plutil -extract "$1" raw -o - "$stage/release.json" 2>/dev/null; }
version=$(json_field tag_name) || fail 'release metadata has no tag_name'
valid_tag "$version" || fail 'release metadata has an unsafe tag_name'
[ "$requested_version" = latest ] || [ "$requested_version" = "$version" ] || fail 'release metadata tag does not match requested version'
# Enumerate the actual assets, rejecting duplicates and missing entries.
asset_url= sums_url= i=0
while name=$(json_field "assets.$i.name"); do
    case "$name" in
        "$asset"|SHA256SUMS)
            url=$(json_field "assets.$i.browser_download_url") || fail 'asset URL missing'
            case "$url" in https://github.com/*/releases/download/*) ;; *) fail 'untrusted asset origin' ;; esac
            source_repo=${url#https://github.com/}
            source_repo=${source_repo%%/releases/download/*}
            [ "$(printf '%s' "$source_repo" | tr 'A-Z' 'a-z')" = "$(printf '%s' "$repo" | tr 'A-Z' 'a-z')" ] || fail 'asset is from a different repository'
            expected=https://github.com/$source_repo/releases/download
            if [ "$url" != "$expected/$(encode_tag "$version")/$name" ] && \
               [ "$url" != "$expected/$version/$name" ]; then
                fail "untrusted release asset URL: $name"
            fi
            if [ "$name" = "$asset" ]; then
                [ -z "$asset_url" ] || fail 'duplicate executable asset'
                asset_url=$url
            else
                [ -z "$sums_url" ] || fail 'duplicate SHA256SUMS asset'
                sums_url=$url
            fi ;;
    esac
    i=$((i + 1))
done
[ -n "$asset_url" ] && [ -n "$sums_url" ] || fail 'release must contain the exact executable and SHA256SUMS assets'
fetch "$sums_url" "$stage/SHA256SUMS"
# Accept standard sha256sum text/binary markers, with an exact basename only.
sha= count=0
while IFS= read -r line || [ -n "$line" ]; do
    line=${line%$'\r'}
    if [[ "$line" =~ ^([[:xdigit:]]{64})\ ([\ \*])([^[:space:]]+)$ ]]; then
        if [ "${BASH_REMATCH[3]}" = "$asset" ]; then
            sha=${BASH_REMATCH[1]}; count=$((count + 1))
        fi
    elif [[ "$line" = *"$asset"* ]]; then
        fail 'malformed executable checksum entry'
    fi
done < "$stage/SHA256SUMS"
[ "$count" -eq 1 ] || fail 'SHA256SUMS must contain exactly one valid executable checksum'
sha=$(printf '%s' "$sha" | tr 'A-F' 'a-f')
owner_dir=$cache/${repo%/*}
repository_dir=$owner_dir/${repo#*/}
version_dir=$repository_dir/$version
install_dir=$version_dir/$target
for dir in "$owner_dir" "$repository_dir" "$version_dir" "$install_dir"; do
    [ ! -L "$dir" ] || fail 'cache subdirectory must not be a symlink'
    mkdir -p "$dir"
    chmod 700 "$dir"
done
executable=$install_dir/$asset
[ ! -L "$executable" ] || fail 'cached executable must not be a symlink'
[ ! -e "$executable" ] || [ -f "$executable" ] || fail 'cached executable is not a regular file'
hash_file() { local result; result=$(shasum -a 256 < "$1") || return 1; printf '%s' "${result%% *}"; }
if [ "$force" -eq 1 ] || [ ! -f "$executable" ] || [ "$(hash_file "$executable")" != "$sha" ]; then
    payload_stage=$(mktemp -d "$install_dir/.stage.XXXXXXXX")
    fetch "$asset_url" "$payload_stage/$asset"
    [ "$(hash_file "$payload_stage/$asset")" = "$sha" ] || fail 'executable SHA256 mismatch'
    chmod 700 "$payload_stage/$asset"
    # Stage in the destination directory to guarantee a same-filesystem rename.
    mv -f "$payload_stage/$asset" "$executable"
fi
chmod 700 "$executable"
# JSON escaping without jq/Python, including all ASCII control characters.
json_string() {
    local s=$1 c n j LC_ALL=C
    printf '"'
    for ((j=0; j<${#s}; j++)); do
        c=${s:j:1}
        case "$c" in
            '"') printf '\\"' ;; \\) printf '\\\\' ;;
            *) printf -v n '%d' "'$c"
               if [ "$n" -ge 0 ] && [ "$n" -lt 32 ]; then printf '\\u%04x' "$n"; else printf '%s' "$c"; fi ;;
        esac
    done
    printf '"'
}
printf '{"status":"ok","repository":'; json_string "$repo"
printf ',"version":'; json_string "$version"
printf ',"target":'; json_string "$target"
printf ',"executable":'; json_string "$executable"
case "$executable" in "$agent_dir/"*) printf ',"executable_relative":'; json_string "./${executable#"$agent_dir"/}" ;; esac
printf ',"sha256":'; json_string "$sha"
printf '}\n'
