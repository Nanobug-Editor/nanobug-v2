"""Measure the ordinary Release editor with real installed packages on Windows."""

import argparse
import ctypes as C
from ctypes import wintypes as W
import json
import math
import pathlib
import shutil
import subprocess
import sys
import time
import zipfile

ROOT = pathlib.Path(__file__).resolve().parents[3]
EDITOR = ROOT / "dist/Nanobug.exe"
sys.dont_write_bytecode = True
sys.path.insert(0, str(ROOT / "experiments" / "plugin-probe"))
import check_windows as native


class Memory(C.Structure):
    _fields_ = [("cb", W.DWORD), ("page_faults", W.DWORD)] + [
        (name, C.c_size_t) for name in
        ("peak_ws", "ws", "peak_paged", "paged", "peak_nonpaged", "nonpaged", "commit", "peak_commit", "private")
    ]


memory_info = native.api(native.KERNEL, "K32GetProcessMemoryInfo", W.BOOL, W.HANDLE, C.POINTER(Memory), W.DWORD)

STARTUP_CASES = {
    "zero": ("empty", "unmatched", 0, 0, 0),
    "fifty_unmatched": ("fifty", "unmatched", 50, 0, 50),
    "fifty_ten_active": ("fifty", "active", 50, 10, 40),
}


def shift_key(hwnd, vk):
    """Send the entire chord together so a focus change cannot leave Shift held."""
    layout = native.keyboard_layout(native.window_pid(hwnd, None))
    scan = native.scan_code(vk, 0, layout)
    assert scan, f"no scan code for key {vk:#x}"
    native.inject(hwnd, [
        native.keyboard_event(0x10),
        native.keyboard_event(0, scan, 8),
        native.keyboard_event(0, scan, 10),
        native.keyboard_event(0x10, flags=2),
    ])
    time.sleep(0.03)


def private_mib(pid):
    handle = native.open_process(0x0400 | 0x0010, False, pid)
    if not handle:
        raise C.WinError(C.get_last_error())
    try:
        memory = Memory()
        memory.cb = C.sizeof(memory)
        if not memory_info(handle, C.byref(memory), memory.cb):
            raise C.WinError(C.get_last_error())
        return memory.private / (1024 * 1024)
    finally:
        native.close_handle(handle)


def wait_event(path, name, predicate=lambda _: True, after=0, timeout=15):
    return native.until(
        lambda: next((row for row in native.events(path)
                      if row["event"] == name and row["at_us"] > after and predicate(row["data"])), None),
        name,
        timeout=timeout,
    )


def draft_matches(path, matches):
    """Poll a draft while the Windows writer may still be replacing its file."""
    try:
        return matches(path.read_text(encoding="utf-8"))
    except (FileNotFoundError, PermissionError):
        return False


def launch(folder, trace, profile, project, installed, ready, unmatched, passive=False):
    args = [EDITOR, "--plugin-profile", profile, "--plugin-trace", trace]
    if project is not None:
        args.append(project)
    trace.unlink(missing_ok=True)
    process = subprocess.Popen(args, cwd=folder)
    try:
        created = native.created_us(process)
        core = wait_event(trace, "core_ready", lambda value: value["editable"] == (project is not None),
                          after=created - 1)
        hwnd = native.until(lambda: native.window_for(process.pid), "owned native window")
        def edit():
            native.posted_key(hwnd, 0x49)
            began = time.time_ns() // 1000
            native.posted_text(hwnd, "x")
            edited = wait_event(trace, "document_revision", lambda value: value["revision"] > 0, after=began - 1)
            return (edited["at_us"] - began) / 1000

        edit_ms = None if passive or project is None else edit()
        checked = wait_event(
            trace, "session_update",
            lambda state: state["installed"] == installed and state["ready"] == ready and state["unmatched"] == unmatched
            and state["workers"] == ready and state["modules"] == ready + unmatched,
        )
        if passive:
            assert not any(item["event"] == "document_revision" and item["data"]["revision"] > 0
                           for item in native.events(trace))
            if project is not None:
                edit_ms = edit()
        host_memory = private_mib(process.pid)
        worker_memory = sum(private_mib(pid) for pid in checked["data"]["worker_pids"])
        result = {
            "core_ready_ms": (core["at_us"] - created) / 1000,
            "rules_and_modules_ready_ms": (checked["at_us"] - created) / 1000,
            "host_private_mib": host_memory,
            "plugin_workers_private_mib": worker_memory,
        }
        if edit_ms is not None:
            result["first_posted_edit_ms"] = edit_ms
            native.posted_key(hwnd, 0x1B)
            native.posted_key(hwnd, 0x55)
            wait_event(trace, "document_revision", lambda value: value["revision"] > 1)
        if ready:
            first_module = wait_event(trace, "session_update", lambda value: value["ready"] > 0)
            result["first_module_ready_ms"] = (first_module["at_us"] - created) / 1000
        return result
    finally:
        native.stop(process)


def measure(folder, samples, cache_state="prepared", passive=False):
    modes = STARTUP_CASES
    if cache_state != "prepared":
        modes = {f"fifty_ten_active_compiled_{cache_state}": STARTUP_CASES["fifty_ten_active"]}
    if passive:
        modes = {
            "fifty_start_page": ("fifty", None, 50, 0, 0),
            "fifty_ten_active_passive": STARTUP_CASES["fifty_ten_active"],
            "fifty_ten_active_empty_document": ("fifty", "empty-document", 50, 10, 40),
        }
    rows = []
    for sample in range(samples + 1):
        for mode, (profile, project, installed, ready, unmatched) in modes.items():
            trace = folder / f"{mode}-{sample:03d}.jsonl"
            profile = folder / profile
            if cache_state != "prepared":
                fresh = folder / f"compiled-{cache_state}-{sample:03d}"
                fresh.mkdir()
                shutil.copytree(profile / "packages", fresh / "packages")
                shutil.copy2(profile / "installed.json", fresh / "installed.json")
                if cache_state == "invalid":
                    compiled = fresh / "compiled"
                    shutil.copytree(profile / "compiled", compiled)
                    entries = [entry for entry in compiled.rglob("*") if entry.is_file()]
                    assert entries, "fixture has no prepared compilation cache"
                    for entry in entries:
                        entry.write_bytes(b"invalid test cache")
                profile = fresh
            project = folder / project if project is not None else None
            result = launch(folder, trace, profile, project, installed, ready, unmatched, passive)
            if sample:
                rows.append({"mode": mode, "sample": sample, **result})
    summary = {}
    for mode in modes:
        group = [row for row in rows if row["mode"] == mode]
        summary[mode] = {}
        for key in group[0].keys() - {"mode", "sample"}:
            values = sorted(row[key] for row in group)
            summary[mode][key] = {f"p{p}": values[math.ceil(len(values) * p / 100) - 1] for p in (50, 95, 99)}
    return {"samples_per_group": samples, "system_cache": "warm",
            "compiled_cache": cache_state,
            "runs": rows, "summary": summary}


def cold_sample(folder, boot):
    """Record one first post-restart launch per Windows boot, using pre-staged fixtures."""
    if boot <= 0:
        raise ValueError("Boot must be the Windows boot time as a positive FILETIME")
    output = folder / "cold-results"
    output.mkdir(exist_ok=True)
    existing = sorted(output.glob("*.json"))
    record = output / f"{boot}.json"
    if record.exists():
        raise ValueError("This boot already has a sample. Restart Windows for the next one.")

    cases = (*STARTUP_CASES, "fifty_ten_active_compiled_missing")
    cycle, position = divmod(len(existing), len(cases))
    mode = cases[position]
    if mode in STARTUP_CASES:
        profile, project, installed, ready, unmatched = STARTUP_CASES[mode]
    else:
        profile, project, installed, ready, unmatched = (f"cold-missing-{cycle + 1:02d}", "active", 50, 10, 40)
        if (folder / profile / "compiled").exists():
            raise ValueError(f"{profile} already has compiled data. Use a fresh pre-staged profile on the next boot.")
    if not (folder / profile).is_dir() or not (folder / project / "README.md").is_file():
        raise FileNotFoundError(f"Cold-start fixture missing: {profile} or {project}")

    trace = output / f"{boot}-{mode}.jsonl"
    row = {"boot_filetime": boot, "mode": mode, "round": cycle + 1, "trace": trace.name, "status": "started"}
    # Reserve the boot before opening the editor. An interrupted launch remains visible.
    with record.open("x", encoding="utf-8") as stream:
        json.dump(row, stream)

    def save():
        pending = record.with_suffix(".tmp")
        pending.write_text(json.dumps(row, indent=2) + "\n", encoding="utf-8")
        pending.replace(record)

    print(f"Cold-start round {cycle + 1}: {mode}", flush=True)
    try:
        row.update(launch(folder, trace, folder / profile, folder / project, installed, ready, unmatched))
    except Exception as error:
        row.update(status="failed", error=f"{type(error).__name__}: {error}")
        save()
        raise
    row["status"] = "measured"
    save()
    print(json.dumps(row, indent=2))
    print(f"Report: {record}")


def edit_during_activation(folder):
    """Measure the document commit path while two projects prepare their rules."""
    trace = folder / "concurrent-input.jsonl"
    trace.unlink(missing_ok=True)
    process = subprocess.Popen(
        [EDITOR, "--plugin-profile", folder / "fifty",
         "--plugin-trace", trace, folder / "active", folder / "unmatched"], cwd=folder,
    )
    try:
        wait_event(trace, "core_ready", lambda value: value["editable"])
        hwnd = native.until(lambda: native.window_for(process.pid), "owned native window")
        native.posted_key(hwnd, 0x49)
        commits = []
        for revision in range(1, 121):
            began = time.time_ns() // 1000
            native.posted_text(hwnd, "x")
            edited = wait_event(trace, "document_revision",
                                lambda value: value["revision"] == revision, after=began - 1)
            commits.append({"at_us": edited["at_us"], "latency_ms": (edited["at_us"] - began) / 1000})
            time.sleep(0.015)
        checked = wait_event(trace, "session_update", lambda state:
                             state["ready"] == 10 and state["unmatched"] == 90
                             and state["modules"] == 100 and state["workers"] == 10)
        overlapping = [commit["latency_ms"] for commit in commits if commit["at_us"] < checked["at_us"]]
        assert overlapping, "no committed input overlapped rule/module preparation"
        native.posted_key(hwnd, 0x1B)
        native.posted_key(hwnd, 0x55)
        wait_event(trace, "document_revision", lambda value: value["revision"] > len(commits))
        values = sorted(commit["latency_ms"] for commit in commits)
        summary = {f"p{p}": values[math.ceil(len(values) * p / 100) - 1] for p in (50, 95, 99)}
        return {"edits_before_all_ready": len(overlapping), "commits": commits,
                "summary_ms": summary, "max_overlap_ms": max(overlapping)}
    finally:
        native.stop(process)


def crash_draft(process, hwnd, trace, path, folder):
    """Compare continuous native commits with the draft left by an abrupt editor exit."""
    def saved():
        data = json.loads(path.read_text(encoding="utf-8"))
        (value,) = (text for plugin in data["inputs"].values()
                    for project in plugin.values() for text in project.values())
        return value

    initial = saved()
    commits = []
    last_saved = 0
    progress = []
    for count in range(1, 46):
        sent = time.time_ns() // 1000
        native.text(hwnd, "~")
        accepted = wait_event(trace, "manager_input", lambda row: row["surface_input"], after=sent - 1)
        commits.append(accepted["at_us"])
        text = saved()
        persisted = len(text) - len(initial)
        assert last_saved <= persisted <= count and text == initial + "~" * persisted
        if persisted > last_saved:
            progress.append(persisted)
            last_saved = persisted
        time.sleep(0.045)
    process.kill()
    process.wait(timeout=5)
    exited = time.time_ns() // 1000
    text = saved()
    persisted = len(text) - len(initial)
    assert last_saved <= persisted <= len(commits) and text == initial + "~" * persisted
    assert len(progress) >= 2, "draft did not advance during continuous input"
    loss_ms = (exited - commits[persisted]) / 1000 if persisted < len(commits) else 0
    result = {"confirmed": len(commits), "retained": persisted,
              "intermediate_persisted_counts": progress, "oldest_lost_input_ms": loss_ms,
              "commits_at_us": commits, "exit_at_us": exited}
    (folder / "summary-draft-crash.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
    assert loss_ms <= 1000, f"lost a confirmed draft input older than one second: {loss_ms:.1f} ms"
    return {"confirmed": len(commits), "retained": persisted, "oldest_lost_input_ms": loss_ms}


def smoke(folder, await_focus=0, pinyin=False, grid=False, caret=False, draft_crash=False):
    project = folder / "smoke-project"
    project.mkdir(exist_ok=True)
    (project / "README.md").write_text("ordinary editor fixture\n", encoding="utf-8")
    kind = "grid" if grid else "panel"
    archive = folder / f"workspace-{kind}.zip"
    demo = ROOT / "plugins/sdk/examples"
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as output:
        output.write(demo / "detect.js", "detect.js")
        output.write(demo / ("workspace-grid.toml" if grid else "nanobug-plugin.toml"), "nanobug-plugin.toml")
        module = f"workspace_{kind}.wasm"
        output.write(ROOT / "target/wasm32-unknown-unknown/release/examples" / module, module)
    trace = folder / "ui-smoke.jsonl"
    profile = folder / "smoke-profile"
    trace.unlink(missing_ok=True)
    process = subprocess.Popen(
        [EDITOR, "--plugin-dev-profile", profile, "--plugin-trace", trace, project],
        cwd=folder,
    )
    try:
        wait_event(trace, "core_ready")
        if await_focus:
            hwnd = native.until(lambda: native.window_for(process.pid), "owned native window")
            native.set_foreground(hwnd)
            print(f"Waiting for the test window to be foreground (PID {process.pid})", flush=True)
            native.until(lambda: native.foreground() == hwnd, "test editor foreground focus", await_focus)
        else:
            hwnd = native.focus_editor(process)
        time.sleep(0.15)

        def left(shift=False):
            # An extended arrow scan code keeps the IME from seeing a numpad key.
            keys = [native.keyboard_event(0, 0x4B, 9), native.keyboard_event(0, 0x4B, 11)]
            if shift:
                keys = [native.keyboard_event(0x10), *keys, native.keyboard_event(0x10, flags=2)]
            native.inject(hwnd, keys)
            time.sleep(0.03)

        def compose(surface, after=0):
            for code in (0x4E, 0x49):
                native.key(hwnd, code)
            return wait_event(trace, "plugin_preedit",
                              lambda value: value["units"] >= 2 and value["surface"] == surface,
                              after=after)

        def cancel_prompt(preedit, next_key):
            # Queue the first command with Escape to catch delayed IME detachment.
            layout = native.keyboard_layout(native.window_pid(hwnd, None))
            keys = []
            for vk in (0x1B, next_key):
                scan = native.scan_code(vk, 0, layout)
                assert scan, f"no scan code for key {vk:#x}"
                keys.extend((native.keyboard_event(0, scan, 8), native.keyboard_event(0, scan, 10)))
            native.inject(hwnd, keys)
            return wait_event(trace, "manager_input", lambda value: value["open"] and not value["prompt"],
                              after=preedit["at_us"], timeout=3)

        if pinyin:
            hkl = native.keyboard_layout(native.window_pid(hwnd, None))
            assert hkl & 0xFFFF == 0x0804, f"Microsoft Pinyin must be active for the test editor (HKL {hkl:#x})"
        native.key(hwnd, 0x20)
        shift_key(hwnd, 0x50)
        opened = wait_event(trace, "manager_open")
        shift_key(hwnd, 0xBF)
        help_open = wait_event(trace, "manager_input", lambda value: value["help"], after=opened["at_us"])
        native.key(hwnd, 0x1B)
        wait_event(trace, "manager_input", lambda value: value["open"] and not value["help"],
                   after=help_open["at_us"])
        native.key(hwnd, 0xBF)
        search_opened = wait_event(trace, "manager_input", lambda value: value["prompt"],
                                   after=help_open["at_us"])
        if pinyin:
            cancelled = cancel_prompt(compose(False, after=search_opened["at_us"]), 0x32)
            assert cancelled["data"]["search_bytes"] == 0
            wait_event(trace, "manager_input", lambda value: value["page"] == 1, after=cancelled["at_us"])
            native.key(hwnd, 0x31)
            native.key(hwnd, 0xBF)
            wait_event(trace, "manager_input", lambda value: value["prompt"],
                       after=cancelled["at_us"])
        native.text(hwnd, "panel")
        wait_event(trace, "manager_input", lambda value: value["search_bytes"] == 5)
        native.key(hwnd, 0x1B)
        wait_event(trace, "manager_input", lambda value: not value["prompt"] and value["search_bytes"] == 0)
        native.key(hwnd, 0x33)
        native.key(hwnd, 0x0D)
        if pinyin:
            prompt = wait_event(trace, "manager_input", lambda value: value["prompt"] and value["page"] == 2)
            cancelled = cancel_prompt(compose(False, after=prompt["at_us"]), 0x0D)
            wait_event(trace, "manager_input", lambda value: value["prompt"] and value["page"] == 2,
                       after=cancelled["at_us"])
        native.text(hwnd, str(archive))
        native.key(hwnd, 0x0D)
        wait_event(trace, "session_update", lambda value: value["review"])
        native.key(hwnd, 0x0D)
        installed = wait_event(trace, "session_update", lambda value: value["installed"] == 1 and value["surfaces"] == 1)
        native.key(hwnd, 0x31)
        native.key(hwnd, 0xBF)
        search = wait_event(trace, "manager_input", lambda value: value["prompt"] and value["page"] == 0,
                            after=installed["at_us"])
        native.text(hwnd, "workspace")
        typed = wait_event(trace, "manager_input", lambda value: value["prompt"] and value["search_bytes"] == 9,
                           after=search["at_us"])
        native.key(hwnd, 0x0D)
        listed = wait_event(trace, "manager_input", lambda value: not value["prompt"] and value["search_bytes"] == 9,
                            after=typed["at_us"])
        selection = listed["data"]["selection"]
        for back in (0x51, 0x1B):  # q and Escape both preserve the filtered list.
            native.key(hwnd, 0x0D)
            detail = wait_event(trace, "manager_input", lambda value: value["details"], after=listed["at_us"])
            native.key(hwnd, back)
            listed = wait_event(trace, "manager_input", lambda value: value["open"] and not value["details"]
                                and value["search_bytes"] == 9 and value["selection"] == selection,
                                after=detail["at_us"])
        native.key(hwnd, 0x0D)
        detail = wait_event(trace, "manager_input", lambda value: value["details"], after=listed["at_us"])
        native.key(hwnd, 0x32)
        native.key(hwnd, 0x0D)
        focused = wait_event(trace, "manager_input", lambda value: value["surface_input"], after=detail["at_us"])
        if pinyin:
            compose(True, after=focused["at_us"])
            native.key(hwnd, 0x20)
            candidate = profile / "drafts.json"
            native.until(lambda: draft_matches(candidate, lambda text: any(
                "\u4e00" <= char <= "\u9fff" for char in text)),
                         "Pinyin candidate commit in Canvas input")
        native.text(hwnd, "示例👋")
        draft = profile / "drafts.json"
        native.until(lambda: draft_matches(draft, lambda text: "示例👋" in text), "saved Canvas input")
        if caret:
            left()
            native.text(hwnd, "中")
            native.until(lambda: draft_matches(draft, lambda text: "示例中👋" in text), "grapheme caret insertion")
            left(shift=True)
            native.text(hwnd, "文")
            native.until(lambda: draft_matches(draft, lambda text: "示例文👋" in text), "selected grapheme replacement")
        if draft_crash:
            return crash_draft(process, hwnd, trace, draft, folder)
        native.key(hwnd, 0x1B)
        return {"zip": "installed", "surface": kind, "draft": "saved", "pinyin": pinyin, "caret": caret}
    finally:
        native.stop(process)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("fixture", type=pathlib.Path)
    parser.add_argument("--editor", type=pathlib.Path, default=EDITOR)
    parser.add_argument("--samples", type=int, default=20)
    parser.add_argument("--smoke", action="store_true")
    scenarios = parser.add_mutually_exclusive_group()
    scenarios.add_argument("--compiled-miss", action="store_true")
    scenarios.add_argument("--compiled-invalid", action="store_true")
    scenarios.add_argument("--passive", action="store_true", help="check startup and wait for activation before input")
    scenarios.add_argument("--cold-boot", type=int, metavar="FILETIME", help="record one first launch for this Windows boot")
    parser.add_argument("--await-focus", type=int, default=0, help="allow a separate native test tool to focus the smoke window")
    parser.add_argument("--pinyin", action="store_true", help="also exercise the active Microsoft Pinyin IME")
    parser.add_argument("--grid", action="store_true", help="smoke the character-grid example")
    parser.add_argument("--caret", action="store_true", help="verify grapheme insertion and selection in the smoke test")
    parser.add_argument("--draft-crash", action="store_true", help="kill the test editor during continuous draft input")
    parser.add_argument("--concurrent-input", action="store_true", help="type during two-project plugin activation")
    args = parser.parse_args()
    if args.samples < 1:
        parser.error("samples must be positive")
    if args.draft_crash and (not args.smoke or args.caret):
        parser.error("--draft-crash requires --smoke and an unmodified caret")
    folder = args.fixture.resolve(strict=True)
    EDITOR = args.editor.resolve(strict=True)
    if args.cold_boot is not None:
        if args.smoke or args.concurrent_input:
            parser.error("--cold-boot requires an ordinary project launch")
        cold_sample(folder, args.cold_boot)
        sys.exit(0)
    if args.smoke:
        result = smoke(folder, args.await_focus, args.pinyin, args.grid, args.caret, args.draft_crash)
        print(json.dumps(result))
        sys.exit(0)
    if args.concurrent_input:
        result = edit_during_activation(folder)
        (folder / "summary-concurrent-input.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
        print(json.dumps({key: value for key, value in result.items() if key != "commits"}))
        sys.exit(0)
    cache_state = "missing" if args.compiled_miss else "invalid" if args.compiled_invalid else "prepared"
    result = measure(folder, args.samples, cache_state, args.passive)
    filename = ("summary-compiled-miss.json" if args.compiled_miss else
                "summary-compiled-invalid.json" if args.compiled_invalid else
                "summary-passive.json" if args.passive else "summary.json")
    (folder / filename).write_text(json.dumps(result, indent=2), encoding="utf-8")
    print(json.dumps(result["summary"], indent=2))
