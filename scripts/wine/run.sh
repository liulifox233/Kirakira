#!/usr/bin/env bash
#
# wine-run — provision a Wine environment, run the real KiriKiri engine under
# it, and drive it from a plain non-shell session.
#
# Why this exists: our own engine cannot prove save-file interoperability
# against the reference implementation, and the games' Windows plugins
# (PackinOne.dll, layerExImage.dll, ...) are otherwise only understood through
# decompilation.  This script runs the *official* krkrz build (the only one
# published upstream) on the session's display (or on an Xvfb it starts when
# there is none, as in CI), on a scratch copy of a game tree,
# and can reproduce a reference bookmark container written by the real
# Plug-in.  M142's report is the long-form write-up; this header is the
# operator-facing summary.
#
# Usage:
#   scripts/wine/run.sh doctor
#   scripts/wine/run.sh setup
#   scripts/wine/run.sh smoke
#   scripts/wine/run.sh game <game-dir> [--name <n>] [--seconds <s>] [--keep]
#   scripts/wine/run.sh shot <out.png>
#   scripts/wine/run.sh click <x> <y> | key <keys> | dialogs | dismiss | stop
#   scripts/wine/run.sh save-harness [--packinone parquet|shoujo] [--thumb <jpg>]
#
# The script re-enters `nix develop <repo>#wine` by itself when Wine is not on
# PATH, so no dev shell is required to run it.  Everything it writes lives
# under KIRA_WINE_ROOT (default ${TMPDIR:-/tmp}/kirakira-wine); the read-only
# game directories are only ever referenced through symlinks.
#
# Environment:
#   KIRA_WINE_ROOT      scratch root (prefix, engine, game trees, artifacts)
#   KIRA_WINE_DISPLAY   X display used for the runs.  Defaults to $DISPLAY
#                       when the session has one (this machine is not a
#                       headless VM: drive the real display and the window is
#                       visible/clickable), to :97 (an Xvfb this script
#                       starts) otherwise, which is the CI case.
#                       Screenshots need an X display: `import` fails on this
#                       machine's Wayland session (:0) for the root window and
#                       for a window alike, so `shot` fails loudly there and
#                       `game` warns instead of silently skipping the picture;
#                       point KIRA_WINE_DISPLAY at :97 when one is wanted.
#   KIRA_WINE_ENGINE    engine release name to require (default krkrz_20171225)
#   KRKRZ_RELEASE_URL   engine release URL override
#   KRKRZ_RELEASE_SHA256 engine release .7z sha-256 (verified on download)
#
# Provenance of what gets downloaded (recorded here because the mission
# wanted it reproducible, not improvised):
#   * Engine: krkrz 1.4.0 rev 2, upstream GitHub release
#       https://github.com/krkrz/krkrz/releases/download/1.4.0r2/krkrz_20171225r2.7z
#     sha256 a58da24a2e14eac102f0fd8bf66106b22ad238d17a317b8d44b65d68efb574df
#     The archive carries tvpwin32.exe (md5 da073263556e695f47fe1fc9923c9dad,
#     "Kirikiri Z Executable core /1.4.0.8 (Compiled on Dec 25 2017)").
#     This is the newest *official* krkrz Windows binary that exists: the
#     upstream release page has nothing newer (checked 2026-09-13).
#
# What the games need and this engine does not have (measured, see the
# mission report for transcripts):
#   * 少女世界的生存之道 and GINKA ship WebP images with .png/.jpg names
#     (RIFF....WEBP magic); the 2017 engine's image loader only knows
#     PNG/JPEG/BMP, so the title screen fails with "PNG Read Error /Not a PNG
#     file".  Their own engine builds (a 2024 krkr fork, per the game's own
#     banner) decode WebP through newer codecs.
#   * PARQUET ships plain PNGs but its KAGEX calls newer native methods
#     ("Member \"getCurrentMessageColor\" does not exist"), so it stops during
#     KAG initialisation.
#   * Consequently the real *game* does not reach a title screen here today.
#     The plugin-level harness below is what still produces reference data.
#
# The bookmark harness (save-harness) is the M142 payoff: it runs the real
# engine with the game's own PackinOne.dll and calls
#   Scripts.saveDataPack(name, data, digest, thumb)
# exactly the way KAGEX's BookMarkIO_DataPack.save() does, with 少女世界's
# parameters (saveDataMode = "z", digest.iv = System.title, ext = "jpg").
# It writes and then reads back savedata_cn/data0.jpg, which proves the real
# writer and reader agree on the container; the produced file has the
# data_anchor.ksd shape (no thumbnail, see the note it prints).
#
# CI note: `scripts/wine/run.sh setup && scripts/wine/run.sh smoke` is the
# headless base a CI job needs — nix provides Wine/Xvfb, the engine is pinned
# by hash, and the smoke test needs no game assets (a scratch startup.tjs
# makes the engine boot and exit 0; when the game's PackinOne.dll is staged
# next to the engine it also writes and re-reads a container through it).
#
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$script_dir/../.." && pwd)

KIRA_WINE_ROOT=${KIRA_WINE_ROOT:-${TMPDIR:-/tmp}/kirakira-wine}
KIRA_WINE_DISPLAY=${KIRA_WINE_DISPLAY:-${DISPLAY:-:97}}
KIRA_WINE_ENGINE=${KIRA_WINE_ENGINE:-krkrz_20171225}
KRKRZ_RELEASE_URL=${KRKRZ_RELEASE_URL:-https://github.com/krkrz/krkrz/releases/download/1.4.0r2/krkrz_20171225r2.7z}
KRKRZ_RELEASE_SHA256=${KRKRZ_RELEASE_SHA256:-a58da24a2e14eac102f0fd8bf66106b22ad238d17a317b8d44b65d68efb574df}
KRKRZ_EXE_MD5=da073263556e695f47fe1fc9923c9dad

prefix=$KIRA_WINE_ROOT/prefix
engine_root=$KIRA_WINE_ROOT/engine
games_root=$KIRA_WINE_ROOT/games
artifacts=$KIRA_WINE_ROOT/artifacts
run_dir=$KIRA_WINE_ROOT/run

export WINEPREFIX=$prefix
export WINEDEBUG=${WINEDEBUG:--all}
export WINEDLLOVERRIDES='mscoree,mshtml='
export DISPLAY=$KIRA_WINE_DISPLAY

die() { printf 'wine-run: %s\n' "$*" >&2; exit 1; }
# Status messages go to stderr so command substitutions like
# `engine_dir=$(fetch_engine)` capture the value and nothing else.
note() { printf 'wine-run: %s\n' "$*" >&2; }

# The script re-enters itself inside `nix develop`, so the original argument
# list has to survive that hop: inside a function, "$@" is the function's own
# arguments, not the script's.
cli_args=("$@")

usage() {
    # The whole header is the usage text; print every leading comment line
    # (shebang aside) so nothing — the caveats, the harness paragraph, the CI
    # note — is cut off.
    awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"
}

# ---------------------------------------------------------------------------
# environment
# ---------------------------------------------------------------------------

# Re-enter the opt-in wine dev shell when the tools are missing.  `nix
# develop` is what pins wine/Xvfb/xdotool/ImageMagick/p7zip/noto-cjk; running
# without it is not supported because none of them are expected on PATH.
ensure_shell() {
    if command -v wine >/dev/null 2>&1 && command -v Xvfb >/dev/null 2>&1; then
        return 0
    fi
    command -v nix >/dev/null 2>&1 \
        || die "neither wine nor nix is on PATH; install nix or run inside nix develop $repo_root#wine"
    note "entering 'nix develop $repo_root#wine'"
    exec nix develop "$repo_root#wine" --command env \
        KIRA_WINE_ROOT="$KIRA_WINE_ROOT" KIRA_WINE_DISPLAY="$KIRA_WINE_DISPLAY" \
        KIRA_WINE_ENGINE="$KIRA_WINE_ENGINE" \
        "${BASH_SOURCE[0]}" "${cli_args[@]}"
}

display_up() { xdpyinfo -display "$KIRA_WINE_DISPLAY" >/dev/null 2>&1; }

start_display() {
    display_up && return 0
    mkdir -p "$run_dir"
    note "starting Xvfb on $KIRA_WINE_DISPLAY"
    nohup Xvfb "$KIRA_WINE_DISPLAY" -screen 0 1920x1080x24 \
        >"$run_dir/xvfb.log" 2>&1 &
    for _ in $(seq 1 50); do
        display_up && return 0
        sleep 0.2
    done
    die "Xvfb did not come up on $KIRA_WINE_DISPLAY (see $run_dir/xvfb.log)"
}

# Wine's own font set is Latin-only, so every CJK glyph in a game renders as a
# box; the shell ships Noto CJK and we drop it into the prefix.  The registry
# replacements point the font names these games ask for at it.
install_fonts() {
    local fonts="$prefix/drive_c/windows/Fonts"
    mkdir -p "$fonts"
    if [ -n "${KIRA_WINE_FONT_DIR:-}" ] && [ -d "$KIRA_WINE_FONT_DIR/opentype/noto-cjk" ]; then
        cp -n "$KIRA_WINE_FONT_DIR"/opentype/noto-cjk/*.ttc "$fonts/" 2>/dev/null || true
    fi
    for face in 'MS Gothic' 'MS UI Gothic' 'MS PGothic' 'SimHei' 'SimSun' 'Microsoft YaHei'; do
        wine reg add 'HKCU\Software\Wine\Fonts\Replacements' /v "$face" /d 'Noto Sans CJK SC' /f >/dev/null
    done
}

wineboot_once() {
    start_display
    install_fonts
    if [ ! -d "$prefix/drive_c/windows/syswow64" ] || [ -z "$(ls -A "$prefix/drive_c/windows/syswow64" 2>/dev/null)" ]; then
        note "initialising wine prefix (first run creates the 32-bit syswow64 tree; ~1-3 min)"
        wine wineboot -u
    fi
}

# ---------------------------------------------------------------------------
# engine
# ---------------------------------------------------------------------------

engine_exe_matches() { # <engine dir>
    local exe=$1/tvpwin32.exe got
    [ -f "$exe" ] || return 1
    got=$(md5sum "$exe" | cut -d' ' -f1)
    [ "$got" = "$KRKRZ_EXE_MD5" ]
}

engine_tarball_matches() { # <tarball>
    echo "$KRKRZ_RELEASE_SHA256  $1" | sha256sum -c - >/dev/null 2>&1
}

fetch_engine() {
    local tarball=$engine_root/krkrz_20171225r2.7z
    local dir=$engine_root/$KIRA_WINE_ENGINE
    mkdir -p "$engine_root"
    # Cached exe: still re-check its md5, so a tampered or half-extracted tree
    # is replaced instead of silently used.
    if [ -f "$dir/tvpwin32.exe" ] && ! engine_exe_matches "$dir"; then
        note "cached engine exe failed its md5 check; re-extracting"
        rm -rf "$dir"
    fi
    if [ -f "$dir/tvpwin32.exe" ]; then
        echo "$dir"
        return 0
    fi
    # Cached archive: a partial download from an earlier run fails the
    # checksum, so say that and fetch again rather than reporting a mismatch.
    if [ -f "$tarball" ] && ! engine_tarball_matches "$tarball"; then
        note "cached engine archive failed its checksum; downloading again"
        rm -f "$tarball"
    fi
    if [ ! -f "$tarball" ]; then
        note "downloading official krkrz release ($KRKRZ_RELEASE_URL)"
        rm -f "$tarball.part"
        curl -fL --retry 3 -o "$tarball.part" "$KRKRZ_RELEASE_URL" \
            || die "engine download failed: $KRKRZ_RELEASE_URL (network problem?)"
        mv "$tarball.part" "$tarball"
    fi
    engine_tarball_matches "$tarball" \
        || die "engine archive checksum mismatch: $tarball (expected $KRKRZ_RELEASE_SHA256); delete it and retry"
    rm -rf "$dir"
    7z x -y -o"$engine_root" "$tarball" >/dev/null
    [ -f "$dir/tvpwin32.exe" ] || die "engine archive layout changed: no tvpwin32.exe under $dir"
    engine_exe_matches "$dir" \
        || die "tvpwin32.exe md5 mismatch (expected $KRKRZ_EXE_MD5): $dir/tvpwin32.exe"
    echo "$dir"
}

# Scratch game tree: archives are symlinked out of the read-only game
# directory, save directories are real (the engine writes there), and the
# engine exe is copied in because the engine treats the exe's directory as the
# project root when no data.xp3 is next to it.
prepare_game_tree() {
    local game_dir=$1 name=$2 engine_dir=$3
    local tree=$games_root/$name
    game_dir=$(readlink -f -- "$game_dir")
    [ -d "$game_dir" ] || die "no such game directory: $game_dir"
    mkdir -p "$tree"
    cp -f "$engine_dir/tvpwin32.exe" "$tree/"
    local f base
    for f in "$game_dir"/*.xp3; do
        [ -e "$f" ] || continue
        base=$(basename -- "$f")
        ln -sfn "$f" "$tree/$base"
    done
    for f in "$game_dir"/patch.tjs "$game_dir"/plugin; do
        [ -e "$f" ] || continue
        ln -sfn "$f" "$tree/$(basename -- "$f")"
    done
    mkdir -p "$tree/savedata" "$tree/savedata_cn"
    echo "$tree"
}

# ---------------------------------------------------------------------------
# running
# ---------------------------------------------------------------------------

# Dialog dismissal: these games open modal "Information" boxes (piracy
# notices, error reports) that block the boot; without a window manager the
# boxes never get input focus on their own, but a targeted
# `xdotool key --window <id> Return` activates the default button.
dismiss_dialogs() {
    local wid
    for wid in $(xdotool search --name '^Information$' 2>/dev/null || true); do
        note "dismissing dialog $wid"
        xdotool key --window "$wid" --clearmodifiers Return 2>/dev/null || true
    done
}

run_engine() {
    local tree=$1 log=$2 seconds=${3:-120} name=${4:-game}
    local engine_pid dismiss_pid
    local project_arg=""
    # The engine picks data.xp3 next to the exe by itself, which is how the
    # games are meant to be launched; only the bare harness (no archive) needs
    # the project directory spelled out, or it would open the folder picker.
    [ -f "$tree/data.xp3" ] || project_arg="."
    ( while :; do dismiss_dialogs; sleep 3; done ) &
    dismiss_pid=$!
    ( cd "$tree" && exec wine ./tvpwin32.exe $project_arg ) >"$log" 2>&1 &
    engine_pid=$!
    local i
    for i in $(seq 1 "$seconds"); do
        sleep 1
        kill -0 "$engine_pid" 2>/dev/null || break
    done
    kill "$dismiss_pid" 2>/dev/null || true
    if kill -0 "$engine_pid" 2>/dev/null; then
        note "engine still running after ${seconds}s (t=$name)"
    fi
    echo "$engine_pid"
}

stop_all() { wineserver -k 2>/dev/null || true; }

# Screen capture differs per display flavour: on an Xvfb screen the root
# window holds the pixels and `import -window root` works, while on this
# machine's Wayland session (:0) ImageMagick's X capture fails for the root
# window *and* for a specific window — XWayland does not hand those pixels
# out.  So: try the root, fall back to the engine's window, and when both
# fail say so instead of pretending a screenshot happened (on :0 use the
# session's own screenshot tool if a picture is needed).
capture() { # <out.png>
    local out=$1 wid
    if import -window root "$out" 2>/dev/null; then
        return 0
    fi
    wid=$(xdotool search --class 'tvpwin32' 2>/dev/null | head -1)
    if [ -n "$wid" ] && import -window "$wid" "$out" 2>/dev/null; then
        note "captured the engine window ($wid) instead of the root screen on $KIRA_WINE_DISPLAY"
        return 0
    fi
    return 1
}

# ---------------------------------------------------------------------------
# bookmark harness
# ---------------------------------------------------------------------------

# The harness is a startup.tjs (see scripts/wine/bookmark-harness.tjs) run in
# a bare engine directory: no game tree, no KAG.  It links the game's own
# PackinOne.dll and exercises saveDataPack/loadDataPack with the parameters
# 少女世界's KAGEX uses.
save_harness() {
    local build=parquet thumb="" want_exe_pack=0
    while [ $# -gt 0 ]; do
        case $1 in
            --packinone) build=$2; shift 2 ;;
            --thumb) thumb=$2; shift 2 ;;
            *) die "save-harness: unknown option $1" ;;
        esac
    done
    local harness=$KIRA_WINE_ROOT/harness
    local engine_dir
    engine_dir=$(fetch_engine)
    wineboot_once
    mkdir -p "$harness/savedata_cn" "$artifacts" "$run_dir"
    cp -f "$engine_dir/tvpwin32.exe" "$harness/"
    case $build in
        parquet)
            cp -f /home/ruri/games/PARQUET/PARQUET/plugin/PackinOne.dll "$harness/PackinOne.dll" ;;
        shoujo|ginka)
            # 少女世界 data.xp3>others/PackinOne.dll == GINKA plugin/PackinOne.dll
            cp -f /home/ruri/games/GINKA/plugin/PackinOne.dll "$harness/PackinOne.dll" ;;
        *) die "save-harness: --packinone must be parquet|shoujo" ;;
    esac
    cp -f "$script_dir/bookmark-harness.tjs" "$harness/startup.tjs"
    note "PackinOne.dll md5: $(md5sum "$harness/PackinOne.dll" | cut -d' ' -f1)"
    stop_all
    ( cd "$harness" && wine ./tvpwin32.exe . ) >"$run_dir/save-harness.log" 2>&1
    local written=$harness/savedata_cn/data0.jpg
    [ -f "$written" ] || die "harness produced no save; see $run_dir/save-harness.log"
    local out=$artifacts/data0-$build.ksd
    cp -f "$written" "$out"
    if [ -n "$thumb" ]; then
        # The pack the harness writes has no thumbnail (the plugin's
        # makeDataPackThumb rejects every layer shape a bare engine can build;
        # see the report).  Prepend a real baseline JPEG to reconstruct the
        # data0.jpg layout.  NOTE: the real reader *rejected* such files in
        # M142's tests, so treat the thumbnail prefix as unverified.
        cat "$thumb" "$out" > "$artifacts/data0-$build-thumb.djpg"
        note "wrote $artifacts/data0-$build-thumb.djpg (thumbnail prefix unverified)"
    fi
    note "wrote $out ($(stat -c %s "$out") bytes)"
    grep -a 'M142' "$run_dir/save-harness.log" || true
}

# ---------------------------------------------------------------------------
# commands
# ---------------------------------------------------------------------------

cmd_doctor() {
    ensure_shell
    echo "wine:        $(wine --version) ($(command -v wine))"
    echo "wine store:  $(readlink -f "$(command -v wine)" | sed 's#/bin/.*##')"
    echo "xvfb:        $(command -v Xvfb)"
    echo "scratch:     $KIRA_WINE_ROOT"
    echo "display:     $KIRA_WINE_DISPLAY"
    if [ -d "$prefix" ]; then
        echo "prefix:      $prefix ($(du -sh "$prefix" | cut -f1))"
    else
        echo "prefix:      (not created yet; run 'setup')"
    fi
    local wine_pkg
    wine_pkg=$(readlink -f "$(command -v wine)" | sed 's#/bin/wine##')
    if command -v nix >/dev/null 2>&1 && [ -d "$wine_pkg" ]; then
        echo "wine closure: $(nix path-info -Sh "$wine_pkg" 2>/dev/null | awk '{print $2, $3}') ($(du -sh "$wine_pkg" 2>/dev/null | cut -f1) for the package tree alone)"
    fi
}

cmd_setup() {
    ensure_shell
    wineboot_once
    local engine_dir
    engine_dir=$(fetch_engine)
    note "engine: $(basename "$engine_dir")/tvpwin32.exe ($(stat -c %s "$engine_dir/tvpwin32.exe") bytes)"
    note "prefix: $prefix ($(du -sh "$prefix" | cut -f1))"
}

cmd_smoke() {
    ensure_shell
    wineboot_once
    local scratch=$run_dir/smoke engine_dir
    engine_dir=$(fetch_engine)
    mkdir -p "$scratch/savedata_cn"
    cp -f "$engine_dir/tvpwin32.exe" "$scratch/"
    cat >"$scratch/startup.tjs" <<'TJS'
// CI-friendly: with the game's plug-in staged this writes and re-reads a real
// container through the reference implementation, then exits 0.
if (Storages.isExistentStorage("PackinOne.dll")) {
    Plugins.link("PackinOne.dll");
    var data = %[ "id" => "smoke" ];
    var digest = %[ "compress" => 1, "cryptmode" => 1, "iv" => "smoke", "ext" => "jpg" ];
    digest.seed = Scripts.makeDataPackDigest(data, 305419896, "smoke");
    Scripts.saveDataPack("savedata_cn/smoke.ksd", data, digest, void);
    var back = Scripts.loadDataPack("savedata_cn/smoke.ksd");
    Debug.message("smoke: container written and read back, id=" + back.id);
} else {
    Debug.message("smoke: no PackinOne.dll staged, engine-only check");
}
System.exit();
TJS
    # hello.exe: a real 32-bit PE that wine must run, built by nix (cached).
    local hello
    hello=$(nix build --no-link --print-out-paths nixpkgs#pkgsCross.mingw32.hello 2>/dev/null)/bin/hello.exe
    [ -x "$hello" ] || die "could not obtain the mingw hello.exe for the smoke test"
    note "hello.exe: $hello"
    ( cd "$scratch" && wine "$hello" ) | tee "$run_dir/smoke-hello.log"
    grep -q 'Hello, world' "$run_dir/smoke-hello.log" || die "hello.exe did not print on wine"
    cp -f /home/ruri/games/GINKA/plugin/PackinOne.dll "$scratch/PackinOne.dll" 2>/dev/null || true
    rm -f "$scratch/savedata_cn/smoke.ksd"
    start_display
    ( cd "$scratch" && timeout 90 wine ./tvpwin32.exe . ) >"$run_dir/smoke-engine.log" 2>&1 || true
    grep -q 'Loading startup script' "$run_dir/smoke-engine.log" \
        || die "engine did not load startup.tjs; see $run_dir/smoke-engine.log"
    grep -q 'smoke: ' "$run_dir/smoke-engine.log" \
        || die "startup.tjs did not run to completion; see $run_dir/smoke-engine.log"
    if [ -f "$scratch/savedata_cn/smoke.ksd" ]; then
        grep -q 'container written and read back' "$run_dir/smoke-engine.log" \
            || die "saveDataPack wrote a file but the read-back did not run; see $run_dir/smoke-engine.log"
        note "smoke ok: hello.exe printed, engine booted, PackinOne wrote and re-read $(stat -c %s "$scratch/savedata_cn/smoke.ksd") bytes"
    else
        note "smoke ok: hello.exe printed, engine booted and ran startup.tjs (no PackinOne.dll staged, so no container was written)"
    fi
}

cmd_game() {
    ensure_shell
    [ $# -ge 1 ] || die "game needs a directory"
    local game_dir=$1; shift
    local name seconds keep
    name=$(basename -- "$game_dir"); seconds=120; keep=0
    while [ $# -gt 0 ]; do
        case $1 in
            --name) name=$2; shift 2 ;;
            --seconds) seconds=$2; shift 2 ;;
            --keep) keep=1; shift ;;
            *) die "game: unknown option $1" ;;
        esac
    done
    wineboot_once
    local engine_dir tree log
    engine_dir=$(fetch_engine)
    tree=$(prepare_game_tree "$game_dir" "$name" "$engine_dir")
    log=$run_dir/$name.log
    mkdir -p "$run_dir"
    note "game tree: $tree (read-only originals are only symlinked)"
    note "log: $log; screenshots: $run_dir/$name-t*.png"
    local pid
    pid=$(run_engine "$tree" "$log" "$seconds" "$name")
    if ! capture "$run_dir/$name-final.png"; then
        note "warning: no screenshot on $KIRA_WINE_DISPLAY (ImageMagick's X capture failed for the root and the engine window; on a Wayland session use the session's screenshot tool)"
    fi
    if [ "$keep" = 0 ]; then stop_all; fi
    note "done; window list follows"
    xdotool search --name '.' getwindowname %@ 2>/dev/null | grep -v '^$' || true
}

cmd_shot() {
    ensure_shell; start_display
    local out=${1:?shot needs an output path}
    capture "$out" || die "could not capture $KIRA_WINE_DISPLAY (ImageMagick's X capture failed for the root and the engine window; on a Wayland session use the session's screenshot tool)"
    note "wrote $out"
}
cmd_click() { ensure_shell; start_display; xdotool mousemove "${1:?click needs x}" "${2:?click needs y}" click 1; }
cmd_key() { ensure_shell; start_display; xdotool key --clearmodifiers "${1:?key needs a key}"; }
cmd_dialogs() { ensure_shell; start_display; xdotool search --name '.' getwindowname %@ 2>/dev/null | grep -v '^$' || true; }
cmd_dismiss() { ensure_shell; start_display; dismiss_dialogs; }
cmd_stop() { ensure_shell; stop_all; note "wine server stopped"; }

case "${1:-}" in
    -h|--help|'') usage; exit 0 ;;
    doctor) shift; cmd_doctor "$@" ;;
    setup) shift; cmd_setup "$@" ;;
    smoke) shift; cmd_smoke "$@" ;;
    game) shift; cmd_game "$@" ;;
    shot) shift; cmd_shot "$@" ;;
    click) shift; cmd_click "$@" ;;
    key) shift; cmd_key "$@" ;;
    dialogs) shift; cmd_dialogs "$@" ;;
    dismiss) shift; cmd_dismiss "$@" ;;
    stop) shift; cmd_stop "$@" ;;
    save-harness) shift; ensure_shell; save_harness "$@" ;;
    *) usage >&2; die "unknown command: $1" ;;
esac
