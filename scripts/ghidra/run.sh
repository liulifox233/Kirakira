#!/usr/bin/env bash
#
# ghidra-run — analyze a native binary with Ghidra headless and export the
# decompiled C, one file per function.
#
# Usage:
#   scripts/ghidra/run.sh <binary> <outdir>
#
#   <outdir>/decompiled/<va>_<name>.c   decompiled C, one file per function
#   <outdir>/decompiled/index.tsv       va, name, file, outcome, detail
#   <outdir>/decompiled/strings.tsv     va, data type, text for every string
#                                       of three characters or more
#   <outdir>/headless.log               analyzeHeadless output for the run
#
# strings.tsv matters because Ghidra cannot define a string shorter than four
# characters (StringsAnalyzer$MinStringLen starts at LEN_4), and the E-mote
# plugins address their property-name pool ("opa", "fx", ...) through pointers
# that decompile to DAT_xxxxxxxx; the table resolves those addresses to text.
#
# The whole pass — import, auto-analysis, per-function decompilation — runs
# through `nix shell`, so a shell with neither Ghidra nor Java on PATH can
# run it, and nothing in the project's flake or default dev shell changes.
#
# PITFALL — project path: Ghidra headless refuses any project path element
# starting with '.' ("Path element starting with '.' is not permitted",
# NamingUtilities.checkName), which rules out every Kirakira worktree
# because they live under .tower/worktrees/.  The scratch project therefore
# defaults to ${TMPDIR:-/tmp}/ghidra-headless/<binary-name> and never touches
# the checkout; point GHIDRA_PROJECT_ROOT elsewhere if you want a different
# scratch root, but nowhere below a dot-directory.
#
# Env (all optional):
#   NIXPKGS_GHIDRA_FLAKE   flake reference for the package (default nixpkgs#ghidra)
#   GHIDRA_HEADLESS_BIN    headless entry point inside the shell (default
#                          ghidra-analyzeHeadless; upstream calls it
#                          analyzeHeadless, nixpkgs prefixes every binary)
#   GHIDRA_PROJECT_ROOT    scratch project root (default ${TMPDIR:-/tmp}/ghidra-headless)
#   GHIDRA_ANALYSIS_TIMEOUT  auto-analysis cap per file, seconds (default 1800).
#                          A cap that is hit is reported at the end of the run:
#                          a cut analysis still exports, but the export can be
#                          missing functions.  motionplayer_nod3d.dll needed
#                          more than 900s on a loaded 32-core box (it managed
#                          499s idle).
#   GHIDRA_DECOMPILE_TIMEOUT per-function decompiler timeout, seconds (default 60)
#   GHIDRA_REUSE=1         re-decompile the project from a previous run instead
#                          of importing and analyzing again
#
set -euo pipefail

usage() {
    cat <<'EOF'
usage: run.sh <binary> <outdir>

Runs Ghidra headless over <binary> and exports per-function decompiled C into
<outdir> (see the header of this script for the exact artefacts and env vars).
EOF
}

die() { printf 'ghidra-run: %s\n' "$*" >&2; exit 1; }

case "${1:-}" in
    -h | --help | '')
        usage
        [ -n "${1:-}" ] || exit 2
        exit 0
        ;;
esac
[ $# -eq 2 ] || {
    usage >&2
    exit 2
}

binary=$1
outdir=$2

[ -f "$binary" ] || die "no such file: $binary"
binary=$(readlink -f -- "$binary")

mkdir -p -- "$outdir"
outdir=$(readlink -f -- "$outdir")

command -v nix >/dev/null 2>&1 \
    || die "nix not found on PATH (needs nix-command + flakes)"

flakeref=${NIXPKGS_GHIDRA_FLAKE:-nixpkgs#ghidra}
headless_bin=${GHIDRA_HEADLESS_BIN:-ghidra-analyzeHeadless}
analysis_timeout=${GHIDRA_ANALYSIS_TIMEOUT:-1800}
decompile_timeout=${GHIDRA_DECOMPILE_TIMEOUT:-60}
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)

bin_name=$(basename -- "$binary")

# Scratch project — never below .tower (see header).
proj_root=${GHIDRA_PROJECT_ROOT:-${TMPDIR:-/tmp}/ghidra-headless}
proj_dir="$proj_root/$bin_name"
proj_name=project
mkdir -p -- "$proj_dir"

if [ "${GHIDRA_REUSE:-0}" = 1 ] && [ -e "$proj_dir/$proj_name.gpr" ]; then
    # The last run's import is still in the project: decompile it again
    # instead of paying for import + auto-analysis a second time.  -noanalysis
    # matters here — processing an existing program re-runs the analysis pass
    # unless it is asked not to.
    printf 'ghidra-run: reusing project %s (no analysis)\n' "$proj_dir"
    import_args=(-process "$bin_name" -noanalysis)
else
    # Drop any half-written project so the imported program name never
    # collides with an earlier run's copy.
    rm -rf -- "$proj_dir/$proj_name.gpr" "$proj_dir/$proj_name.rep"
    import_args=(-import "$binary")
fi

printf 'ghidra-run: %s -> %s (project %s, nix %s)\n' \
    "$binary" "$outdir/decompiled" "$proj_dir" "$flakeref"

start_seconds=$SECONDS
set +e
nix shell "$flakeref" --command "$headless_bin" "$proj_dir" "$proj_name" \
    "${import_args[@]}" \
    -analysisTimeoutPerFile "$analysis_timeout" \
    -scriptPath "$script_dir/ghidra_scripts" \
    -postScript ExportDecompilation.java "$outdir" "$decompile_timeout" \
    2>&1 | tee "$outdir/headless.log"
status=${PIPESTATUS[0]}
set -e
[ "$status" -eq 0 ] || die "$headless_bin failed (exit $status); see $outdir/headless.log"

if grep -q 'Analysis timed out' "$outdir/headless.log"; then
    printf 'ghidra-run: warning: auto-analysis was cut off at %ss; the export may be missing functions\n' \
        "$analysis_timeout" >&2
    printf 'ghidra-run: raise GHIDRA_ANALYSIS_TIMEOUT for a complete pass\n' >&2
fi

printf 'ghidra-run: done in %ds; %s\n' "$((SECONDS - start_seconds))" "$outdir/decompiled/index.tsv"
