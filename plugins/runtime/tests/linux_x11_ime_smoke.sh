#!/usr/bin/env bash
# Run on an isolated X11 display with its own XDG directories and Fcitx instance.
set -euo pipefail

if [[ $# != 3 ]]; then
    printf 'Usage: %s <nanobug-executable> <workspace-panel.zip> <new-output-dir>\n' "$0" >&2
    exit 2
fi
: "${DISPLAY:?Start the private X11 display first}"
if [[ -n "${WAYLAND_DISPLAY:-}${WAYLAND_SOCKET:-}" || "${XMODIFIERS:-}" != '@im=fcitx' ]]; then
    printf 'Run with XMODIFIERS=@im=fcitx and no Wayland display.\n' >&2
    exit 2
fi
binary=$(realpath -e -- "$1")
archive=$(realpath -e -- "$2")
mkdir -- "$3"
output=$(realpath -- "$3")
mkdir -- "$output/project" "$output/profile"
printf 'plugin IME fixture\n' > "$output/project/README.md"
trace="$output/trace.jsonl"

"$binary" --plugin-dev-profile "$output/profile" --plugin-trace "$trace" \
    "$output/project" > "$output/editor.log" 2>&1 &
app=$!
trap 'kill "$app" 2>/dev/null || true; wait "$app" 2>/dev/null || true' EXIT
window=$(timeout 12 xdotool search --sync --onlyvisible --pid "$app" | head -n 1)
xdotool windowfocus --sync "$window"

key() {
    [[ "$(xdotool getwindowfocus)" == "$window" ]]
    xdotool key --clearmodifiers "$@"
}

await_event() {
    local pattern="$1" after="${2:-0}"
    for _ in {1..130}; do
        if tail -n "+$((after + 1))" "$trace" 2>/dev/null | grep -- "$pattern" >/dev/null; then
            return
        fi
        kill -0 "$app" 2>/dev/null || break
        sleep .05
    done
    printf 'Missing trace event %s after line %s\n' "$pattern" "$after" >&2
    tail -n 12 "$trace" "$output/editor.log" >&2
    return 1
}

rows() { wc -l < "$trace"; }

await_pinyin() {
    for _ in {1..40}; do
        if [[ "$(fcitx5-remote) $(fcitx5-remote -n)" == '2 pinyin' ]]; then
            return
        fi
        sleep .05
    done
    printf 'Fcitx Pinyin did not become active in the private test window.\n' >&2
    return 1
}

await_event '"event":"core_ready"'
fcitx5-remote -c
key space shift+p 3 Return
await_event '"prompt":true,"page":2'
xdotool type --clearmodifiers "$archive"
key Return
await_event '"review":true'
key Return
await_event '"surfaces":1'
key 1 slash
await_event '"open":true,"prompt":true,"page":0'
fcitx5-remote -o
fcitx5-remote -s pinyin
await_pinyin

first=$(rows)
key z h o n g
sleep .2
key space
await_event '"search_bytes":3' "$first"
first_exit=$(rows)
key Escape q
await_event '"open":false,"prompt":false' "$first_exit"
[[ "$(fcitx5-remote) $(fcitx5-remote -n)" == '2 pinyin' ]]
printf 'Escape and the first q close search and the manager with Pinyin still active.\n'

reopen=$(rows)
key space shift+p slash
await_event '"open":true,"prompt":true,"page":0' "$reopen"
key z h o n g
sleep .2
key space
await_event '"search_bytes":3' "$reopen"
printf 'Reopened search commits a second Chinese candidate.\n'

unconfirmed=$(rows)
key Escape slash
await_event '"open":true,"prompt":true,"page":0' "$unconfirmed"
key n i
sleep .25
xwininfo -root -tree > "$output/candidate-window.txt"
candidate=$(awk '/Fcitx5 Input Window/ {print $1; exit}' "$output/candidate-window.txt")
xwininfo -id "$candidate" | grep 'Map State: IsViewable' >/dev/null
cancel=$(rows)
key Escape q
await_event '"open":false,"prompt":false' "$cancel"
python3 - "$trace" "$cancel" <<'PY'
from pathlib import Path
import json
import sys

for line in Path(sys.argv[1]).read_text().splitlines()[int(sys.argv[2]):]:
    event = json.loads(line)
    assert event['event'] != 'manager_input' or event['data']['search_bytes'] == 0, event
PY
printf 'Escape cancels a visible candidate and commits no search text.\n'

document=$(rows)
key i
key z h o n g
sleep .2
key space
await_event '"event":"document_revision"' "$document"
key Escape ctrl+s
python3 - "$output/project/README.md" <<'PY'
from pathlib import Path
import sys
from time import sleep

for _ in range(80):
    text = Path(sys.argv[1]).read_text()
    if '\u4e00' <= text[0] <= '\u9fff' and text[1:] == 'plugin IME fixture\n':
        break
    sleep(.05)
else:
    raise AssertionError(repr(text))
PY
printf 'The first i enters insert mode and native save writes exactly one Chinese character.\n'
