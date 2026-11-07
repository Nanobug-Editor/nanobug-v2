# Nanobug V2

[简体中文](README.zh-CN.md)

**Status: In active development.** Features and behavior may change.

Nanobug V2 is a native desktop code editor built with Rust and GPUI. Its character grid brings together a file explorer and a text editor with Vim-style commands, smooth cursor movement and scrolling, and native input method support.

## Run from source

Windows is the currently tested platform. With the Rust toolchain installed, run this from the repository root:

```powershell
cargo run --locked
```

Pass a file or directory after `--` to open it at startup. See the [usage guide (Chinese)](docs/使用指南.md) for keybindings and development notes.
