"""Measure native input through the document, scene, and composed desktop pixels."""

import argparse
import ctypes as C
from ctypes import wintypes as W
import importlib.util
import json
import math
import pathlib
import subprocess
import sys
import threading
import time

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    "nanobug_release_check", pathlib.Path(__file__).with_name("check_windows.py")
)
windows = importlib.util.module_from_spec(spec)
spec.loader.exec_module(windows)

native = windows.native
origin = native.api(native.USER, "ClientToScreen", W.BOOL, W.HWND, C.POINTER(W.POINT))
window_dpi = native.api(native.USER, "GetDpiForWindow", W.UINT, W.HWND)
set_context = native.api(native.USER, "SetProcessDpiAwarenessContext", W.BOOL, C.c_void_p)


class SmallDesktopCapture:
    """Copy just the fixture's cells from the composed desktop into a reusable bitmap."""

    def __init__(self, region):
        self.left, self.top, right, bottom = region
        self.width, self.height = right - self.left, bottom - self.top
        gdi = C.WinDLL("gdi32", use_last_error=True)
        self.copy = native.api(
            gdi, "BitBlt", W.BOOL, W.HDC, C.c_int, C.c_int, C.c_int, C.c_int,
            W.HDC, C.c_int, C.c_int, W.DWORD,
        )
        self.read = native.api(gdi, "GetBitmapBits", C.c_long, W.HANDLE, C.c_long, C.c_void_p)
        self.release = native.api(native.USER, "ReleaseDC", C.c_int, W.HWND, W.HDC)
        self.delete_dc = native.api(gdi, "DeleteDC", W.BOOL, W.HDC)
        self.delete_bitmap = native.api(gdi, "DeleteObject", W.BOOL, W.HANDLE)
        self.select = native.api(gdi, "SelectObject", W.HANDLE, W.HDC, W.HANDLE)
        self.desktop = native.api(native.USER, "GetDC", W.HDC, W.HWND)(None)
        self.memory = native.api(gdi, "CreateCompatibleDC", W.HDC, W.HDC)(self.desktop)
        self.bitmap = native.api(
            gdi, "CreateCompatibleBitmap", W.HANDLE, W.HDC, C.c_int, C.c_int,
        )(self.desktop, self.width, self.height)
        if not all((self.desktop, self.memory, self.bitmap)):
            self.delete_bitmap(self.bitmap)
            self.delete_dc(self.memory)
            self.release(None, self.desktop)
            raise RuntimeError("Could not allocate the desktop sample bitmap")
        self.previous = self.select(self.memory, self.bitmap)
        self.bytes = C.create_string_buffer(self.width * self.height * 4)

    def frame(self):
        import numpy as np

        if not self.copy(
            self.memory, 0, 0, self.width, self.height,
            self.desktop, self.left, self.top, 0x40CC0020,
        ):
            raise C.WinError(C.get_last_error())
        if self.read(self.bitmap, len(self.bytes), self.bytes) != len(self.bytes):
            raise C.WinError(C.get_last_error())
        return np.frombuffer(self.bytes, dtype=np.uint8).reshape(self.height, self.width, 4)[:, :, 2::-1]

    def close(self):
        self.select(self.memory, self.previous)
        self.delete_bitmap(self.bitmap)
        self.delete_dc(self.memory)
        self.release(None, self.desktop)


def wait_foreground(hwnd, timeout):
    """Wait for a stable test window before generating keyboard input."""
    native.set_foreground(hwnd)
    since = None

    def focused():
        nonlocal since
        if native.foreground() != hwnd:
            since = None
            return False
        now = time.monotonic()
        if since is None:
            since = now
        return now - since >= 0.4

    native.until(focused, "foreground timing fixture", timeout)


def input_region(hwnd, scene):
    """Observe two letter cells inside the document and keep captures in memory."""
    corner = W.POINT()
    if not origin(hwnd, C.byref(corner)):
        raise C.WinError(C.get_last_error())
    scale = window_dpi(hwnd) / 96
    x, y = scene["data"]["text_origin"]
    width, height = scene["data"]["cell"]
    left = corner.x + round(x * scale)
    top = corner.y + round(y * scale)
    region = left, top, left + round(width * scale * 3), top + round(height * scale)
    return region, max(1, round(width * scale))


def record(folder, executable, samples, await_focus):
    import numpy as np

    set_context(C.c_void_p(-4))
    trace = folder / "presented-input.jsonl"
    rust, vue = folder / "rust", folder / "vue"
    trace.unlink(missing_ok=True)
    process = subprocess.Popen(
        [executable, "--plugin-dev-profile", folder / "profile", "--plugin-trace", trace, rust, vue],
        cwd=folder,
    )
    capture = None
    updates = None
    finish_updates = threading.Event()
    try:
        windows.wait_event(trace, "core_ready")
        hwnd = native.until(lambda: native.window_for(process.pid), "native editor")
        print(f"Native timing window PID {process.pid}", flush=True)
        wait_foreground(hwnd, await_focus)
        windows.wait_event(trace, "session_update", lambda value: value["ready"] == 2)
        native.key(hwnd, 0x20)
        windows.wait_event(trace, "document_scene")
        windows.shift_key(hwnd, 0x50)
        opened = windows.wait_event(trace, "manager_open")
        native.key(hwnd, 0x0D)
        details = windows.wait_event(trace, "manager_input", lambda value: value["details"], after=opened["at_us"])
        native.key(hwnd, 0x32)
        native.key(hwnd, 0x0D)
        shown = windows.wait_event(trace, "manager_input", lambda value: not value["open"], after=details["at_us"])
        windows.wait_event(trace, "session_update", lambda value: value.get("drawing_revision") is not None,
                           after=shown["at_us"])

        scene = windows.wait_event(trace, "document_scene", lambda value: value["revision"] == 0)
        region, cell = input_region(hwnd, scene)
        capture = SmallDesktopCapture(region)
        client = W.POINT(region[0] + cell // 2, region[1] + 2)
        screen_to_client = native.api(native.USER, "ScreenToClient", W.BOOL, W.HWND, C.POINTER(W.POINT))
        if not screen_to_client(hwnd, C.byref(client)):
            raise C.WinError(C.get_last_error())
        # Click in the document so the bottom panel remains visible but does not take input.
        lparam = (client.y << 16) | client.x
        assert native.post(hwnd, 0x201, 1, lparam)
        assert native.post(hwnd, 0x202, 0, lparam)
        native.posted_key(hwnd, 0x49)
        time.sleep(0.15)

        def pixels(expect_letter):
            frame = capture.frame()[:, :cell]
            # Look for the blue tint of the font ink; the animated caret is green.
            center = frame[3:-3, 3:-1]
            letters = np.count_nonzero((np.min(center, axis=2) > 140)
                                      & (center[:, :, 2].astype(np.int16) - center[:, :, 1] > 9))
            return letters >= 12 if expect_letter else letters <= 8

        native.until(lambda: pixels(False), "blank fixture cell")
        samples_raw = []
        begun = time.time_ns() // 1000
        dependency_changes = []
        background_results = []
        background_errors = []

        def change_dependency():
            # Only Vue's rule changes its result. Wait for each visible transition
            # before writing again so dependency coalescing cannot hide progress.
            gate = vue / "gate.txt"
            staged = vue / "gate.next"
            ready = False
            try:
                while not finish_updates.is_set():
                    staged.write_text("ready" if ready else "blocked", encoding="utf-8")
                    changed = time.time_ns() // 1000
                    try:
                        staged.replace(gate)
                    except PermissionError:
                        finish_updates.wait(0.05)
                        continue
                    dependency_changes.append(changed)
                    while not finish_updates.wait(0.08):
                        completed = next((event for event in native.events(trace)
                                          if event["event"] == "session_update" and event["at_us"] >= changed
                                          and event["data"]["ready"] == (2 if ready else 1)
                                          and event["data"]["unmatched"] == (0 if ready else 1)), None)
                        if completed:
                            background_results.append({"at_us": completed["at_us"], "vue_ready": ready})
                            break
                    ready = not ready
            except Exception as error:
                background_errors.append(error)

        updates = threading.Thread(target=change_dependency)
        updates.start()
        for index in range(samples):
            letter = index % 2 == 0
            assert native.foreground() == hwnd, "fixture lost foreground"
            at_us = time.time_ns() // 1000
            if letter:
                native.posted_text(hwnd, "W")
            else:
                native.posted_key(hwnd, 0x08, settle=False)
            try:
                native.until(lambda: pixels(letter), f"desktop pixels for revision {index + 1}", timeout=2)
            except AssertionError:
                from PIL import Image

                Image.fromarray(capture.frame()).save(folder / "unmatched-document-cells.png")
                print(f"Document sample region {region}, cell width {cell}", file=sys.stderr)
                raise
            displayed = time.time_ns() // 1000
            edited = windows.wait_event(trace, "document_revision", lambda value: value["revision"] == index + 1)
            painted = windows.wait_event(trace, "document_scene", lambda value: value["revision"] == index + 1)
            assert at_us <= edited["at_us"] <= painted["at_us"] <= displayed, (
                f"revision {index + 1}: enqueue={at_us}, document={edited['at_us']}, "
                f"scene={painted['at_us']}, pixels={displayed}"
            )
            samples_raw.append({
                "revision": index + 1,
                "input": "insert" if letter else "backspace",
                "at_us": at_us,
                "document_ms": (edited["at_us"] - at_us) / 1000,
                "scene_ms": (painted["at_us"] - at_us) / 1000,
                "composed_desktop_observed_ms": (displayed - at_us) / 1000,
            })
        finish_updates.set()
        updates.join()
        if background_errors:
            raise background_errors[0]
        elapsed = native.events(trace)
        drawings = {
            event["data"].get("drawing_revision") for event in elapsed
            if event["event"] == "session_update" and begun <= event["at_us"] <= displayed
        } - {None}
        assert len(drawings) >= samples // 4, f"only {len(drawings)} scene revisions during input"
        background_results = [event for event in background_results if event["at_us"] <= displayed]
        assert {event["vue_ready"] for event in background_results} == {False, True}, (
            "the background Vue rule did not complete both transitions during input"
        )

        def percent(key):
            data = sorted(row[key] for row in samples_raw)
            return {f"p{p}": data[math.ceil(samples * p / 100) - 1] for p in (50, 95, 99, 100)}

        return {
            "sample_region_px": region,
            "presented_measurement": "first matching composed desktop capture; includes capture polling, excludes physical display scan-out",
            "scenes_while_editing": len(drawings),
            "other_project_dependency_changes_us": dependency_changes,
            "other_project_rule_results": background_results,
            "summary_ms": {key: percent(key) for key in ("document_ms", "scene_ms", "composed_desktop_observed_ms")},
            "samples": samples_raw,
        }
    finally:
        finish_updates.set()
        if updates is not None:
            updates.join()
        if capture is not None:
            capture.close()
        native.stop(process)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("fixture", type=pathlib.Path)
    parser.add_argument("--editor", type=pathlib.Path, default=windows.EDITOR)
    parser.add_argument("--samples", type=int, default=100)
    parser.add_argument("--await-focus", type=int, default=40)
    options = parser.parse_args()
    if options.samples < 1:
        parser.error("samples must be positive")
    fixture = options.fixture.resolve(strict=True)
    output = record(fixture, options.editor.resolve(strict=True), options.samples, options.await_focus)
    (fixture / "summary-presented-input.json").write_text(json.dumps(output, indent=2), encoding="utf-8")
    print(json.dumps({"scenes_while_editing": output["scenes_while_editing"], **output["summary_ms"]}))
