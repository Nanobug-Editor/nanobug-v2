#!/usr/bin/env bash
# Exercise real Fcitx Pinyin on a disposable Sway compositor.
set -euo pipefail

if [[ $# != 4 ]]; then
    printf 'Usage: %s <nanobug-executable> <workspace-panel.zip> <fcitx-wayland-log> <new-output-dir>\n' "$0" >&2
    exit 2
fi
: "${WAYLAND_DISPLAY:?Start an isolated Sway compositor and Fcitx first}"
: "${SWAYSOCK:?Set the isolated compositor IPC socket}"
if [[ -n "${DISPLAY:-}${WAYLAND_SOCKET:-}" ]]; then
    printf 'Unset DISPLAY and WAYLAND_SOCKET for this test.\n' >&2
    exit 2
fi

binary=$(realpath -e -- "$1")
archive=$(realpath -e -- "$2")
ime_log=$(realpath -e -- "$3")
mkdir -- "$4"
output=$(realpath -- "$4")
mkdir -- "$output/project" "$output/profile"
cp -- "$archive" "$output/project/钟"
printf 'plugin IME fixture\n' > "$output/project/README.md"
trace="$output/trace.jsonl"

wayland-info > "$output/protocols.log"
for protocol in zwp_text_input_manager_v3 zwp_input_method_manager_v2 zwp_virtual_keyboard_manager_v1; do
    grep -Fq "'$protocol'" "$output/protocols.log"
done

WAYLAND_DEBUG=1 "$binary" --plugin-dev-profile "$output/profile" --plugin-trace "$trace" \
    "$output/project" > "$output/editor.log" 2>&1 &
app=$!
trap 'kill "$app" 2>/dev/null || true; wait "$app" 2>/dev/null || true' EXIT

# Validate the recipient immediately before sending each virtual keyboard sequence.
focused() {
    swaymsg -t get_tree | python3 -c '
import json
import sys

def owns_focus(node):
    return (node.get("focused") and node.get("pid") == int(sys.argv[1])) or any(
        owns_focus(child) for key in ("nodes", "floating_nodes") for child in node.get(key, [])
    )

sys.exit(0 if owns_focus(json.load(sys.stdin)) else 1)
' "$app"
}

key() {
    focused
    wtype -s 100 "$@" -s 100
}

await_line() {
    local file="$1" pattern="$2" after="${3:-0}"
    for _ in {1..130}; do
        if tail -n "+$((after + 1))" "$file" 2>/dev/null | grep -- "$pattern" >/dev/null; then
            return
        fi
        kill -0 "$app" 2>/dev/null || break
        sleep .05
    done
    printf 'Missing %s in %s after line %s\n' "$pattern" "$file" "$after" >&2
    tail -n 10 "$trace" "$output/editor.log" >&2
    return 1
}

rows() { wc -l < "$1"; }

await_line "$trace" '"event":"core_ready"'
for _ in {1..40}; do
    focused && break
    sleep .05
done
key -k space -M shift -k p -m shift -s 120 -k 3 -k Return
await_line "$trace" '"prompt":true,"page":2'
await_line "$output/editor.log" 'zwp_text_input_v3@.*\.enable()'
fcitx5-remote -o
fcitx5-remote -s pinyin
for _ in {1..40}; do
    [[ "$(fcitx5-remote) $(fcitx5-remote -n)" == '2 pinyin' ]] && break
    sleep .05
done
[[ "$(fcitx5-remote) $(fcitx5-remote -n)" == '2 pinyin' ]]

# The isolated Pinyin profile selects 钟 for zhong. The exact file checks path
# commitment end to end through inspection and a real plugin install.
key -k z -k h -k o -k n -k g -s 180 -k space
await_line "$output/editor.log" 'zwp_text_input_v3@.*\.commit_string("钟")'
key -k Return
await_line "$trace" '"review":true'
key -k Return
await_line "$trace" '"surfaces":1'
printf 'Chinese ZIP path committed and the ZIP installed.\n'

search=$(rows "$trace")
key -k 1 -k slash
await_line "$trace" '"open":true,"prompt":true,"page":0' "$search"
key -k z -k h -k o -k n -k g -s 180 -k space
await_line "$trace" '"search_bytes":3' "$search"
after_search=$(rows "$trace")
wire_after_search=$(rows "$output/editor.log")
ime_after_search=$(rows "$ime_log")
key -k Escape
await_line "$trace" '"open":true,"prompt":false' "$after_search"
# Wait for the compositor to deactivate Fcitx and for Fcitx to release the keyboard.
await_line "$ime_log" 'zwp_input_method_v2@.*\.deactivate()' "$ime_after_search"
await_line "$ime_log" 'zwp_input_method_keyboard_grab_v2@.*\.release()' "$ime_after_search"
WAYLAND_DEBUG=1 key -k q 2> "$output/exit-keys.log"
await_line "$trace" '"open":false,"prompt":false' "$after_search"
python3 - "$output/editor.log" "$wire_after_search" "$output/exit-keys.log" <<'PY'
from pathlib import Path
import re
import sys

lines = Path(sys.argv[1]).read_text().splitlines()[int(sys.argv[2]):]
keyboard = Path(sys.argv[3]).read_text()
q, = re.findall(r"zwp_virtual_keyboard_v1@\d+\.key\([^,]+,\s*(\d+),\s*1\)", keyboard)
disable = next(i for i, line in enumerate(lines) if re.search(r"text_input_v3@.*\.disable\(", line))
assert any(re.search(rf"wl_keyboard@.*\.key\([^,]+,[^,]+,\s*{q},\s*1\)", line)
           for line in lines[disable + 1:]), "q did not arrive as a Wayland keyboard event"
assert not any('commit_string("q")' in line for line in lines), "q became a late IME commit"
PY
printf 'Search candidate committed. After Escape, the first q arrived as a keyboard command.\n'

reopen=$(rows "$trace")
key -k space -M shift -k p -m shift -s 120 -k slash
await_line "$trace" '"open":true,"prompt":true,"page":0' "$reopen"
key -k n -k i
await_line "$trace" '"event":"plugin_preedit".*"units":2' "$reopen"
cancel=$(rows "$trace")
wire_cancel=$(rows "$output/editor.log")
key -k Escape
await_line "$output/editor.log" 'zwp_text_input_v3@.*\.done(' "$wire_cancel"
await_line "$trace" '"open":true,"prompt":true,"page":0.*"search_bytes":0' "$cancel"
# Fcitx consumes the first Escape when it has a candidate. Once it clears the
# candidate, the next Escape reaches Nanobug and q is an editor command.
ime_after_cancel=$(rows "$ime_log")
key -k Escape
await_line "$trace" '"open":true,"prompt":false' "$cancel"
await_line "$ime_log" 'zwp_input_method_v2@.*\.deactivate()' "$ime_after_cancel"
await_line "$ime_log" 'zwp_input_method_keyboard_grab_v2@.*\.release()' "$ime_after_cancel"
key -k q
await_line "$trace" '"open":false,"prompt":false' "$cancel"
python3 - "$trace" "$cancel" <<'PY'
from pathlib import Path
import json
import sys

for line in Path(sys.argv[1]).read_text().splitlines()[int(sys.argv[2]):]:
    event = json.loads(line)
    assert event["event"] != "manager_input" or event["data"]["search_bytes"] == 0, event
PY
printf 'The IME consumed the first Escape and canceled ni without a commit. The next Escape and q closed the manager.\n'

document=$(rows "$trace")
wire_document=$(rows "$output/editor.log")
key -k i
await_line "$output/editor.log" 'zwp_text_input_v3@.*\.enable()' "$wire_document"
key -k z -k h -k o -k n -k g -s 180 -k space
await_line "$trace" '"event":"document_revision"' "$document"
key -k Escape -s 30 -M ctrl -k s -m ctrl
python3 - "$output/project/README.md" <<'PY'
from pathlib import Path
import sys
from time import sleep

path = Path(sys.argv[1])
for _ in range(80):
    text = path.read_text()
    if text == "钟plugin IME fixture\n":
        break
    sleep(.05)
else:
    raise AssertionError(repr(text))
PY
printf 'The first i entered insert mode, and the document saved exactly one confirmed Chinese character.\n'
