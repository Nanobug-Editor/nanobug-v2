# GPUI Linux backend

Source is the published `gpui-pre-linux` 0.3.10 crate, Apache-2.0, from Zed revision `08cbb88a152c3751a43e853425a70e9f631a5918`. The published crate's SHA-256 is `7c6ffd2dce7b75cce0bb3382994912f84c2a05389a4a236bb8bd72cb098fc083`. The source directory, normalized manifest and Apache notice come from that package. [Published source](https://crates.io/crates/gpui-pre-linux/0.3.10)

Nanobug adds `x11-modal-ime` to `Cargo.toml` and enables it for Linux builds. The modifications are in `src/linux/x11/client.rs` and `src/linux/x11/window.rs`. The backend checks live text focus before forwarding keys to XIM and keeps each key's routing through its release. Escape is handled by the editor immediately and resets the native composition. XIM protocol messages still reach the input method, and the backend checks text focus again before delivering a commit or preedit. Unchanged platform behavior applies without the feature.

The Linux build also enables `wayland-modal-ime`. It checks the live input handler after key and pointer dispatch and flushes the text-input mode change to the compositor immediately. Text and preedit arrive as a batch. The batch can only reach the focused editable field that enabled it. Text commits go to that field as text, so a late one-byte commit cannot run a command after the field closes.

## Reproduce the X11 input test

1. Build Nanobug and `nanobug-plugin-runtime` from the same checkout. Create a temporary editor profile and use a known plugin ZIP with a project-scoped UI.
2. Start a private Xvfb display and a private D-Bus session. Use temporary XDG directories, a Fcitx 5 profile containing `keyboard-us` and `pinyin`, and `XMODIFIERS=@im=fcitx`. Unset `WAYLAND_DISPLAY` and `WAYLAND_SOCKET` for the test process. Start Fcitx before opening the editor on that display.
3. Open the editor with `--plugin-dev-profile <temp-profile> --plugin-trace <trace.jsonl> <test-project>`. Install the ZIP, open the Installed tab and press `/`. Activate Fcitx Pinyin once. Type `zhong` and select a candidate. The trace should show a nonempty search string.
4. Send `xdotool key --clearmodifiers Escape q` while Fcitx Pinyin remains active. The trace should show the search close and then the manager close. Reopen with `Space`, `Shift+P`, `/` and select another Chinese candidate without toggling Fcitx.
5. Open a fresh search, type `ni`, and check that the Fcitx candidate window is mapped. Send `Escape q`. The trace should show the manager close with no committed search text after Escape. Enter the document with `i`, select a Chinese candidate, then save. Only the confirmed character should be added to the file.

The isolated Ubuntu 24.04 test used Fcitx 5.1.7 and Xvfb. [Smoke test](../../plugins/runtime/tests/linux_x11_ime_smoke.sh)

## Reproduce the Wayland input test

1. Build both Release executables from the same checkout. Start a private Sway compositor under a private D-Bus session with `WLR_BACKENDS=headless`, `WLR_HEADLESS_OUTPUTS=1`, `WLR_RENDERER=pixman`, and a short private `XDG_RUNTIME_DIR`. Use a Sway configuration containing `xwayland disable` and `output * resolution 1440x960`.
2. Set `SWAYSOCK` and `WAYLAND_DISPLAY` to that compositor and unset `DISPLAY` and `WAYLAND_SOCKET`. Start Fcitx 5 with `WAYLAND_DEBUG=1 fcitx5 > fcitx-wayland.log 2>&1`, private XDG directories and a profile containing the US keyboard and Pinyin. The test expects the isolated dictionary to select `钟` for `zhong`. Install `wtype` and `wayland-info` in the test environment.
3. Run `bash plugins/runtime/tests/linux_wayland_ime_smoke.sh <nanobug-executable> <workspace-panel.zip> <fcitx-wayland-log> <new-output-dir>`. It installs a ZIP by entering a Chinese path, commits a search candidate, asserts that the first `q` after an editor-handled Escape and Fcitx's keyboard release is a Wayland keyboard event, cancels a Pinyin candidate without a commit, and saves a confirmed Chinese character in the document.

The isolated Ubuntu 24.04 run used Sway 1.9, wlroots 0.17.1, Fcitx 5.1.7 and Pinyin 5.1.3. Fcitx consumes Escape while composing. The first Escape cancels the candidate. The next Escape reaches the editor and leaves the search or insert mode. The compositor reports cleared preedit without the key that caused it. Backspace can produce the same update. The search's Cancel button directly closes the prompt and discards the candidate. If a key arrives before the compositor processes the disable, Fcitx can still receive it. The editor discards any resulting late text instead of executing it as a command. [Fcitx on Wayland](https://fcitx-im.org/wiki/Using_Fcitx_5_on_Wayland/en) · [Wayland text-input-v3 specification](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/blob/main/unstable/text-input/text-input-unstable-v3.xml)

A second isolated Sway run used IBus 1.5.32 and ibus-libpinyin 1.15.8. Escape during composition and Backspace deleting the final Pinyin letter both delivered empty preedit followed by `done`. A confirmed Chinese candidate reached the search. When Escape and `q` were queued together after confirmation, Escape closed the prompt and IBus consumed `q` before releasing its keyboard grab. The first `q` after release closed the manager. [IBus Pinyin key handling](https://github.com/libpinyin/ibus-libpinyin/blob/1.15.8/src/PYPPhoneticEditor.cc#L110-L158)
