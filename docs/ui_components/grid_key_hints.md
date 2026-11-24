# GridKeyHints

`GridKeyHints` 将操作名和按键排成一行，例如 `返回: Esc | 移动: j/k`。最近项目、插件管理器、文件选择器和文件预览在底部用它显示当前模式可用的操作。

```rust
use crate::ui::components::grid_key_hints::GridKeyHints;
use nanobug::i18n;

let hints = GridKeyHints::fit(
    columns.len(),
    [(i18n::text("picker.hint_back"), "Esc"), (i18n::text("picker.hint_move"), "j/k")],
    spacing,
);
frame.key_hints(row, columns.start, &hints, (TEXT, MUTED), spacing);
```

`fit` 按传入顺序保留能完整显示的提示，连同分隔符计算实际列宽。动态操作可用 `new` 和 `push` 逐项加入，`push` 返回 `false` 表示空间不足。`width` 是最终占用的列数，可用于定位同一行中的命令输入区。渲染器用 `LabelStyle::Dimmed` 绘制，两个颜色参数分别用于提示文字和分隔符。

调用方按当前模式提供本地化操作名、按键和优先顺序，并负责相应的按键处理。

[组件入口](../../src/ui/components/grid_key_hints/mod.rs) · [最近项目](../../src/grid/windows/start/projects.rs) · [插件管理器](../../src/grid/windows/plugin_manager/layout.rs) · [文件选择器和预览](../../src/grid/workspace/file_picker/focused.rs)
