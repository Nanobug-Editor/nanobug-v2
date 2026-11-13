# Nanobug 插件 P0 实验

这是在真实 Nanobug 窗口中运行的测试插件。启动完成后，独立 JS 进程请求宿主读取项目根的 `probe-enabled.txt`。内容为 `yes` 时，在独立 Wasmtime 进程中准备该项目的界面。两个匹配项目共享一个 WASM 进程，各有自己的实例。进程故障期间，已确认的草稿由宿主保留。

从此工作区根目录构建，随后运行三个 Windows 原生检查。检查只向这次启动的测试窗口发键，并在独立的 `test-output/plugin-probe` 子目录中写示例文件及记录。

```powershell
cargo build --release -p nanobug -p nanobug-probe-worker --features plugin-probe --locked
python experiments/plugin-probe/check_windows.py smoke
python experiments/plugin-probe/check_windows.py slow-read
python experiments/plugin-probe/check_windows.py ime
python experiments/plugin-probe/check_windows.py measure --samples 20
```

`ime` 使用当前窗口的微软拼音。运行启动测量时保持机器负载平稳。各次运行在输出中列出 `result.json` 和原始时间记录所在目录。

锁屏时可用 `smoke --posted` 验证进程、真实窗口消息和状态恢复。物理按键及候选上屏使用前台检查。

手动体验时，在测试目录放一个 `README.md` 和内容为 `yes` 的 `probe-enabled.txt`，给 `nanobug.exe` 传 `--plugin-probe`、一个尚不存在的 JSONL 路径、测试项目目录。F8 聚焦草稿，Esc 返回正文。F9 结束测试插件进程，F10 运行受限 WASM 忙循环，F11 运行受限 JS 忙循环。只读浏览时也会自动准备插件。普通编辑器启动方式照常使用。

实验代码、协议及证据边界见 [P0 验证报告](../../docs/research/2026-11-12-plugin-p0-results.md)和[绘图清单](../../docs/research/2026-11-12-plugin-p0-gpui.md)。
