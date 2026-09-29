# Helpers shared by the packaging scripts. Source it after setting REPO_ROOT.

die() {
    echo "$(basename "$0"): $*" >&2
    exit 1
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

# Copies the licence texts and the GPL notice into DIR (DIR/LICENSES/, DIR/NOTICE.txt).
copy_licences() {
    local ffmpeg="$1" dir="$2"
    mkdir -p "$dir/LICENSES"
    cp "$ffmpeg"/LICENSES/* "$dir/LICENSES/"
    [ -f "$ffmpeg/SOURCES.txt" ] && cp "$ffmpeg/SOURCES.txt" "$dir/LICENSES/FFmpeg-SOURCES.txt"
    cp "$REPO_ROOT/LICENSE" "$dir/LICENSES/Glass-LICENSE"
    cp "$REPO_ROOT/scripts/package/NOTICE.txt" "$dir/NOTICE.txt"
}
