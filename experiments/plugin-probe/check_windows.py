"""Run native P0 checks against the Release binaries from this worktree."""

import argparse
import ctypes as C
from ctypes import wintypes as W
import json
import math
import pathlib
import subprocess
import tempfile
import time


ROOT = pathlib.Path(__file__).resolve().parents[2]
BIN = ROOT / "target" / "release"
USER = C.WinDLL("user32", use_last_error=True)
KERNEL = C.WinDLL("kernel32", use_last_error=True)


def api(library, name, result, *arguments):
    """Declare the Win32 ABI before passing handles or pointers."""
    function = getattr(library, name)
    function.restype = result
    function.argtypes = arguments
    return function


ENUM_CALLBACK = C.WINFUNCTYPE(W.BOOL, W.HWND, W.LPARAM)
enum_windows = api(USER, "EnumWindows", W.BOOL, ENUM_CALLBACK, W.LPARAM)
visible = api(USER, "IsWindowVisible", W.BOOL, W.HWND)
window_pid = api(USER, "GetWindowThreadProcessId", W.DWORD, W.HWND, C.POINTER(W.DWORD))
window_title = api(USER, "GetWindowTextW", C.c_int, W.HWND, W.LPWSTR, C.c_int)
client_rect = api(USER, "GetClientRect", W.BOOL, W.HWND, C.POINTER(W.RECT))
keyboard_layout = api(USER, "GetKeyboardLayout", W.HKL, W.DWORD)
scan_code = api(USER, "MapVirtualKeyExW", W.UINT, W.UINT, W.UINT, W.HKL)
foreground = api(USER, "GetForegroundWindow", W.HWND)
set_foreground = api(USER, "SetForegroundWindow", W.BOOL, W.HWND)
post = api(USER, "PostMessageW", W.BOOL, W.HWND, W.UINT, W.WPARAM, W.LPARAM)
get_times = api(KERNEL, "GetProcessTimes", W.BOOL, W.HANDLE, *[C.POINTER(W.FILETIME)] * 4)
open_process = api(KERNEL, "OpenProcess", W.HANDLE, W.DWORD, W.BOOL, W.DWORD)
close_handle = api(KERNEL, "CloseHandle", W.BOOL, W.HANDLE)
query_image = api(KERNEL, "QueryFullProcessImageNameW", W.BOOL, W.HANDLE, W.DWORD, W.LPWSTR, C.POINTER(W.DWORD))
terminate = api(KERNEL, "TerminateProcess", W.BOOL, W.HANDLE, W.UINT)


class Keyboard(C.Structure):
    _fields_ = [("vk", W.WORD), ("scan", W.WORD), ("flags", W.DWORD), ("time", W.DWORD), ("extra", C.c_size_t)]


class Mouse(C.Structure):
    _fields_ = [("x", W.LONG), ("y", W.LONG), ("data", W.DWORD), ("flags", W.DWORD), ("time", W.DWORD), ("extra", C.c_size_t)]


class InputData(C.Union):
    _fields_ = [("keyboard", Keyboard), ("mouse", Mouse)]


class Input(C.Structure):
    _fields_ = [("kind", W.DWORD), ("data", InputData)]


send_input = api(USER, "SendInput", W.UINT, W.UINT, C.POINTER(Input), C.c_int)


def window_for(pid):
    """Return a visible window owned by the launched editor PID."""
    found = []

    @ENUM_CALLBACK
    def visit(window, _):
        owner = W.DWORD()
        window_pid(window, C.byref(owner))
        if owner.value == pid and visible(window):
            title = C.create_unicode_buffer(64)
            window_title(window, title, len(title))
            if title.value == "Nanobug V2":
                found.append(window)
        return True

    enum_windows(visit, 0)
    return next(iter(found), None)


def until(read, description, timeout=12):
    """Bound the wait for a native or trace condition."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = read()
        if result:
            return result
        time.sleep(0.01)
    raise AssertionError(f"Timed out waiting for {description}")


def events(path):
    """Read complete JSON lines while the supervisor is still appending."""
    if not path.exists():
        return []
    content = path.read_bytes()
    return [json.loads(line) for line in content.split(b"\n")[:-1] if line.strip()]


def event(path, name, predicate=lambda _: True):
    return until(lambda: next((item for item in events(path) if item["event"] == name and predicate(item["detail"])), None), name)


def created_us(process):
    """Return kernel process creation time in Unix microseconds."""
    created, exited, system, user = (W.FILETIME() for _ in range(4))
    if not get_times(process._handle, *[C.byref(value) for value in (created, exited, system, user)]):
        raise C.WinError(C.get_last_error())
    return ((created.dwHighDateTime << 32) | created.dwLowDateTime) // 10 - 11644473600000000


def keyboard_event(vk, scan=0, flags=0):
    return Input(1, InputData(keyboard=Keyboard(vk, scan, flags, 0, 0)))


def inject(hwnd, strokes):
    """Send only to this test's foreground window. Stop if focus has moved."""
    if foreground() != hwnd:
        raise AssertionError("test editor lost foreground focus")
    array = (Input * len(strokes))(*strokes)
    if send_input(len(array), array, C.sizeof(Input)) != len(array):
        raise C.WinError(C.get_last_error())


def key(hwnd, vk, control=False):
    layout = keyboard_layout(window_pid(hwnd, None))

    def physical(code, up=False):
        scan = scan_code(code, 0, layout)
        assert scan, f"no scan code for key {code:#x}"
        return keyboard_event(0, scan, 8 | (2 if up else 0))

    keys = [physical(vk), physical(vk, up=True)]
    if control:
        keys = [physical(0x11), *keys, physical(0x11, up=True)]
    inject(hwnd, keys)
    time.sleep(0.03)


def text(hwnd, value):
    """Exercise Win32 committed Unicode input, including UTF-16 surrogate pairs."""
    data = value.encode("utf-16-le")
    strokes = []
    for offset in range(0, len(data), 2):
        unit = int.from_bytes(data[offset:offset + 2], "little")
        strokes.extend((keyboard_event(0, unit, 4), keyboard_event(0, unit, 6)))
    inject(hwnd, strokes)


def fixture(folder, name, flag="yes"):
    project = folder / name
    project.mkdir()
    (project / "README.md").write_bytes(b"probe body\n")
    (project / "probe-enabled.txt").write_text(flag, encoding="utf-8")
    return project


def start(trace, projects, baseline=False, delayed=False):
    flag = "--plugin-probe-delay-first" if delayed else "--plugin-probe-baseline" if baseline else "--plugin-probe"
    return subprocess.Popen([BIN / "nanobug.exe", flag, trace, *projects], cwd=ROOT)


def stop(process):
    """Close only the editor launched by this check, with bounded fallback."""
    if process.poll() is None and (hwnd := window_for(process.pid)):
        post(hwnd, 0x10, 0, 0)  # WM_CLOSE
    try:
        process.wait(timeout=2)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


def kill_worker(pid):
    """Inject a real unexpected exit after verifying the PID's exact executable."""
    handle = open_process(0x1000 | 0x0001, False, pid)
    if not handle:
        raise C.WinError(C.get_last_error())
    try:
        path = C.create_unicode_buffer(32768)
        size = W.DWORD(len(path))
        if not query_image(handle, 0, path, C.byref(size)):
            raise C.WinError(C.get_last_error())
        if pathlib.Path(path.value).resolve() != (BIN / "nanobug-probe-worker.exe").resolve():
            raise AssertionError("worker PID does not belong to this probe build")
        if not terminate(handle, 1):
            raise C.WinError(C.get_last_error())
    finally:
        close_handle(handle)


def close_project(hwnd):
    for vk in (0x20, 0x51, 0x51):  # Space, q, q
        key(hwnd, vk)


def focus_editor(process):
    """Allow keyboard input only after the selected test process is foreground."""
    hwnd = until(lambda: window_for(process.pid), "native window")
    if foreground() != hwnd:
        set_foreground(hwnd)
        until(lambda: foreground() == hwnd, "test editor foreground focus", 3)
    return hwnd


def click_editor_body(hwnd):
    """Route a click through the foreground test window, outside the plugin surface."""
    assert foreground() == hwnd
    bounds = W.RECT()
    assert client_rect(hwnd, C.byref(bounds))
    x, y = bounds.right * 3 // 5, bounds.bottom // 4
    lparam = (y << 16) | x
    assert post(hwnd, 0x201, 1, lparam)
    assert post(hwnd, 0x202, 0, lparam)


def edit_body(hwnd, trace, press, enter):
    """Enter Insert with the first command key and type a known prefix."""
    press(hwnd, 0x49)  # i
    event(trace, "editor_mode", lambda d: d["insert"] and d["revision"] == 0)
    enter(hwnd, "saved ")
    body_input = event(trace, "editor_input")
    press(hwnd, 0x1B)
    return body_input


def save_body(hwnd, trace, project):
    """Preserve the original body and save the edit via Ctrl+s."""
    body_input = edit_body(hwnd, trace, key, text)
    key(hwnd, 0x53, control=True)
    until(lambda: (project / "README.md").read_bytes() == b"saved probe body\n", "body save")
    return body_input


def posted_key(hwnd, vk, settle=True):
    """Exercise native key dispatch on the owned HWND when foreground input is unavailable."""
    layout = keyboard_layout(window_pid(hwnd, None))
    scan = scan_code(vk, 0, layout)
    assert post(hwnd, 0x100, vk, 1 | (scan << 16))
    assert post(hwnd, 0x101, vk, 1 | (scan << 16) | 0xC0000000)
    if settle:
        time.sleep(0.03)


def posted_text(hwnd, value):
    """Send committed text to the owned native window without touching desktop focus."""
    data = value.encode("utf-16-le")
    for offset in range(0, len(data), 2):
        assert post(hwnd, 0x102, int.from_bytes(data[offset:offset + 2], "little"), 1)


def smoke(folder, posted=False):
    """Verify two project scopes, native input and an unexpected WASM exit."""
    first = fixture(folder, "first")
    second = fixture(folder, "second")
    trace = folder / "native.jsonl"
    previous = foreground()
    process = start(trace, [first, second])
    try:
        creation = created_us(process)
        core = event(trace, "core_ready", lambda d: d["editable"])
        runtime = event(trace, "runtime_started")
        ready = event(trace, "plugin_frame", lambda d: d["scope"] == 1)
        event(trace, "guest_frame", lambda d: d["instances"] == 2)
        event(trace, "business_result", lambda d: d["result"] == {"Ok": 42})
        assert core["us"] < runtime["us"] <= ready["us"]
        assert len({process.pid, runtime["detail"]["pid"], event(trace, "rules_started")["detail"]["pid"]}) == 3

        hwnd = until(lambda: window_for(process.pid), "native window") if posted else focus_editor(process)
        press = posted_key if posted else key
        enter = posted_text if posted else text
        press(hwnd, 0x77)  # F8
        event(trace, "draft_focus", lambda d: d["focused"])
        enter(hwnd, "中文🙂")
        committed = event(trace, "draft_input", lambda d: d["bytes"] == 10)
        revision = committed["detail"]["revision"]
        event(trace, "guest_frame", lambda d: d["scope"] == 1 and d["revision"] == revision)

        kill_worker(runtime["detail"]["pid"])
        exited = event(trace, "runtime_exited")
        enter(hwnd, "前")
        latest = event(trace, "draft_input", lambda d: d["bytes"] == 13)
        revision = latest["detail"]["revision"]
        layout = event(trace, "draft_layout", lambda d: d["scope"] == 1 and d["revision"] == revision)
        assert layout["detail"]["caret_columns"] > 4
        press(hwnd, 0x1B)  # Esc, return to the normal-mode body
        if posted:
            body_input = edit_body(hwnd, trace, press, enter)
        else:
            body_input = save_body(hwnd, trace, first)

        restarted = event(trace, "runtime_started", lambda d: d["generation"] == 2)
        recovered = event(trace, "plugin_frame", lambda d: d["generation"] == 2 and d["scope"] == 1 and d["revision"] == revision)
        assert restarted["detail"]["pid"] != runtime["detail"]["pid"]
        assert exited["us"] <= latest["us"] <= layout["us"] < restarted["us"]
        assert exited["us"] < body_input["us"] < restarted["us"]
        event(trace, "guest_frame", lambda d: d["generation"] == 2 and d["scope"] == 1 and d["bytes"] == 13)
        discarded = event(trace, "frame_discarded", lambda d: d["scope"] == 1 and d["generation"] == 1)
        assert restarted["us"] <= discarded["us"] < recovered["us"]

        press(hwnd, 0x77)  # F8, back into the host-owned draft
        press(hwnd, 0x79)  # F10, start the fuel-bounded business loop
        busy_started = event(trace, "business_started")
        enter(hwnd, "忙")
        during_work = event(trace, "draft_input", lambda d: d["bytes"] == 16)
        painted_during_work = event(trace, "plugin_frame", lambda d: d["revision"] == during_work["detail"]["revision"])
        busy_wasm = event(trace, "business_result", lambda d: d["spin"] and d["result"] == {"Err": "wasm_budget"})
        assert busy_started["us"] < during_work["us"] <= painted_during_work["us"] < busy_wasm["us"]
        press(hwnd, 0x1B)
        press(hwnd, 0x7A)  # F11
        busy_js = event(trace, "rule_done", lambda d: d["result"] == {"Err": "js_budget"})
        if posted:
            # The existing Save confirmation exercises the disk write without modifier state.
            for vk in (0x20, 0x51, 0x51, 0x59):
                press(hwnd, vk)
            until(lambda: (first / "README.md").read_bytes() == b"saved probe body\n", "body save on close")
        else:
            close_project(hwnd)
        closed = event(trace, "scope_closed", lambda d: d["scope"] == 1 and d["instances"] == 1)
        event(trace, "plugin_frame", lambda d: d["scope"] == 2 and d["generation"] == 2)
        time.sleep(0.5)
        assert not any(item["event"] == "runtime_stopped" for item in events(trace))
        for vk in (0x20, 0x51, 0x51):
            press(hwnd, vk)
        event(trace, "scope_closed", lambda d: d["scope"] == 2 and d["instances"] == 0)
        event(trace, "runtime_stopped", lambda d: d["graceful"])

        return {
            "startup_ms": (core["unix_us"] - creation) / 1000,
            "input_method": "targeted_window_messages" if posted else "physical_scan_codes",
            "local_draft_layout_ms": (layout["us"] - latest["us"]) / 1000,
            "recovery_ms": (recovered["us"] - exited["us"]) / 1000,
            "js_interrupt_ms": busy_js["detail"]["js_us"] / 1000,
            "wasm_interrupt_ms": busy_wasm["detail"]["elapsed_us"] / 1000,
            "wasm_parallel_frame_ms": (painted_during_work["us"] - busy_started["us"]) / 1000,
            "remaining_instances_after_close": closed["detail"]["instances"],
        }
    finally:
        stop(process)
        if previous and previous != foreground():
            set_foreground(previous)


def slow_read(folder, posted=False):
    """Keep the real host read lane occupied after the waiting rule has timed out."""
    first = fixture(folder, "slow-read")
    second = fixture(folder, "fast-read")
    trace = folder / "slow-read.jsonl"
    previous = foreground()
    process = start(trace, [first, second], delayed=True)
    try:
        begin = event(trace, "read_started", lambda d: d["scope"] == 1 and d["delayed"])
        other = event(trace, "guest_frame", lambda d: d["scope"] == 2)
        if posted:
            hwnd = until(lambda: window_for(process.pid), "native window")
            body = edit_body(hwnd, trace, posted_key, posted_text)
        else:
            hwnd = focus_editor(process)
            body = save_body(hwnd, trace, first)
        expired = event(trace, "rule_done", lambda d: d["scope"] == 1 and d["result"] == {"Err": "rule_deadline"})
        completed = event(trace, "read_completed", lambda d: d["scope"] == 1 and d["expired"])
        assert expired["detail"]["read_active"]
        assert begin["us"] < body["us"] < expired["us"] < completed["us"]
        assert other["us"] < expired["us"]
        assert not any(item["event"] == "guest_frame" and item["detail"]["scope"] == 1 for item in events(trace))
        if posted:
            for vk in (0x20, 0x51, 0x51, 0x59):
                posted_key(hwnd, vk)
            until(lambda: (first / "README.md").read_bytes() == b"saved probe body\n", "body save on close")
        return {
            "rule_total_ms": expired["detail"]["total_us"] / 1000,
            "read_ms": (completed["us"] - begin["us"]) / 1000,
            "read_still_active_after_timeout_ms": (completed["us"] - expired["us"]) / 1000,
        }
    finally:
        stop(process)
        if previous and previous != foreground():
            set_foreground(previous)


def ime(folder):
    """Use the active Windows Pinyin IME to check preedit cancellation and candidate commit."""
    project = fixture(folder, "ime-project")
    trace = folder / "ime.jsonl"
    previous = foreground()
    process = start(trace, [project])
    try:
        event(trace, "plugin_frame")
        runtime = event(trace, "runtime_started")
        hwnd = focus_editor(process)
        hkl = keyboard_layout(window_pid(hwnd, None))
        assert hkl & 0xFFFF == 0x0804, f"Microsoft Pinyin must be active for the test editor (HKL {hkl:#x})"
        key(hwnd, 0x77)  # F8
        key(hwnd, 0x4E)  # n
        key(hwnd, 0x49)  # i
        before = event(trace, "draft_preedit", lambda d: d["units"] >= 2)
        kill_worker(runtime["detail"]["pid"])
        event(trace, "runtime_exited")
        click_editor_body(hwnd)  # leave with uncommitted Pinyin via pointer focus
        left = event(trace, "draft_focus", lambda d: not d["focused"])
        assert not any(item["event"] == "draft_input" for item in events(trace))
        key(hwnd, 0x49)  # the very next command enters Insert on the main document
        command = event(trace, "editor_mode", lambda d: d["insert"] and d["revision"] == 0)
        assert before["us"] < left["us"] < command["us"]
        key(hwnd, 0x1B)
        event(trace, "plugin_frame", lambda d: d["generation"] == 2 and d["revision"] == 0)
        key(hwnd, 0x77)  # F8 restores the same native IME
        key(hwnd, 0x4E)
        key(hwnd, 0x49)
        key(hwnd, 0x20)  # select the first Pinyin candidate
        commit = event(trace, "draft_input", lambda d: d["bytes"] == 3)
        event(trace, "plugin_frame", lambda d: d["revision"] == commit["detail"]["revision"])
        return {"preedit_cancelled_during_worker_exit": True, "first_command_insert": True, "candidate_committed_utf8_bytes": 3}
    finally:
        stop(process)
        if previous and previous != foreground():
            set_foreground(previous)


def measure(folder, samples):
    """Alternate warm launches of the same host with and without a matching project rule."""
    project = fixture(folder, "sample")
    rows = []
    for index in range(samples + 1):
        for baseline in (True, False):
            kind = "baseline" if baseline else "plugin"
            trace = folder / f"{kind}-{index:03d}.jsonl"
            process = start(trace, [project], baseline)
            try:
                creation = created_us(process)
                core = event(trace, "core_ready", lambda d: d["editable"])
                row = {"mode": kind, "editable_ms": (core["unix_us"] - creation) / 1000}
                if not baseline:
                    ready = event(trace, "plugin_input_ready")
                    frame = event(trace, "plugin_frame")
                    business = event(trace, "business_result", lambda d: d["result"] == {"Ok": 42})
                    row["plugin_input_ms"] = (ready["unix_us"] - creation) / 1000
                    row["plugin_frame_ms"] = (frame["unix_us"] - creation) / 1000
                    row["business_ms"] = (business["unix_us"] - creation) / 1000
                if index:
                    rows.append(row)
            finally:
                stop(process)
    summaries = {}
    for kind in ("baseline", "plugin"):
        group = [row for row in rows if row["mode"] == kind]
        summaries[kind] = {}
        for field in (field for field in group[0] if field != "mode"):
            values = sorted(row[field] for row in group)
            summaries[kind][field] = {f"p{p}": values[math.ceil(len(values) * p / 100) - 1] for p in (50, 95, 99)}
    return {"samples_per_group": samples, "runs": rows, "summary_ms": summaries}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("smoke", "slow-read", "ime", "measure"))
    parser.add_argument("--posted", action="store_true", help="Use the owned window's messages when the desktop is locked")
    parser.add_argument("--samples", type=int, default=20)
    args = parser.parse_args()
    if args.posted and args.mode not in ("smoke", "slow-read"):
        parser.error("--posted applies to smoke and slow-read")
    if args.samples < 1:
        parser.error("--samples must be positive")
    runs = ROOT / "test-output" / "plugin-probe"
    runs.mkdir(parents=True, exist_ok=True)
    folder = pathlib.Path(tempfile.mkdtemp(prefix=f"{args.mode}-", dir=runs))
    print(f"Trace directory: {folder}", flush=True)
    checks = {"smoke": lambda folder: smoke(folder, args.posted), "slow-read": lambda folder: slow_read(folder, args.posted), "ime": ime}
    result = measure(folder, args.samples) if args.mode == "measure" else checks[args.mode](folder)
    report = json.dumps(result, indent=2)
    (folder / "result.json").write_text(report + "\n", encoding="utf-8")
    print(json.dumps(result["summary_ms"] if args.mode == "measure" else result))


if __name__ == "__main__":
    main()
