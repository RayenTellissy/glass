# Helpers shared by the packaging scripts. Source it after setting REPO_ROOT.

die() {
    echo "$(basename "$0"): $*" >&2
    exit 1
}

# The script's help text: the comment lines HELP_LINES (for example "2,24") of $0.
usage() {
    sed -n "${HELP_LINES}p" "$0" | sed "s/^# \{0,1\}//"
}

usage_error() {
    echo "$(basename "$0"): $*" >&2
    usage >&2
    exit 2
}

# need_value OPTION [VALUE ...]: exits with usage (status 2) unless OPTION has a value.
need_value() {
    [ $# -ge 2 ] && [ -n "$2" ] && [ "${2#-}" = "$2" ] || usage_error "$1 needs a value"
}

# The version in [workspace.package] of the root Cargo.toml.
workspace_version() {
    local v
    v="$(sed -n "/^\[workspace.package\]/,/^\[/s/^version = \"\(.*\)\"/\1/p" "$REPO_ROOT/Cargo.toml")"
    [ -n "$v" ] || die "could not read the workspace version"
    printf "%s" "$v"
}

# A 1 x 1 transparent PNG, for formats that refuse to build without an icon while
# assets/icons/ does not exist yet.
placeholder_png() {
    echo "note: using a placeholder icon for $1" >&2
    printf "%s" "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=" | base64 -d >"$1"
}

# Copies the licence texts and the GPL notice into DIR (DIR/LICENSES/, DIR/NOTICE.txt),
# with the texts of every Rust crate compiled into the app gathered from the crates'
# sources into DIR/LICENSES/third-party-crates.txt (needs cargo and python3).
copy_licences() {
    local ffmpeg="$1" dir="$2"
    mkdir -p "$dir/LICENSES"
    cp "$ffmpeg"/LICENSES/* "$dir/LICENSES/"
    [ -f "$ffmpeg/SOURCES.txt" ] && cp "$ffmpeg/SOURCES.txt" "$dir/LICENSES/FFmpeg-SOURCES.txt"
    cp "$REPO_ROOT/LICENSE" "$dir/LICENSES/Glass-LICENSE"
    cp "$REPO_ROOT/scripts/package/NOTICE.txt" "$dir/NOTICE.txt"
    python3 "$REPO_ROOT/scripts/licences/third-party.py" --check ||
        die "assets/licences/third-party.tsv is out of date; run scripts/licences/third-party.py"
    python3 "$REPO_ROOT/scripts/licences/third-party.py" --texts "$dir/LICENSES/third-party-crates.txt" ||
        die "could not gather the crates' licence texts"
}
