# GridTextField

`GridTextField` 为单行输入维护光标、选区和横向显示起点。文件选择器的过滤、搜索、跳转、新建和重命名，最近项目过滤、设置单行值、插件管理器搜索和开发路径，以及插件宿主绘制的网格输入都使用它。

```rust
use crate::ui::components::grid_text_field::GridTextField;

let mut field = GridTextField::with_limit(usize::MAX);
let mut value = String::from("项目.rs");
field.select_prefix(&value, "项目".len());
field.insert(&mut value, "笔记");
let layout = field.layout(&value, composition, columns.len(), spacing);
frame.text_field(row, columns.clone(), &layout, spacing);
let caret_x = columns.start as f32 + layout.caret_columns();
```

窗口保存 `String`、`GridTextField` 和原生输入法的 `Composition`。按键交给 `edit(&mut value, keystroke)`，返回 `Some(true)` 时更新依赖文本的搜索结果或校验结果。系统上屏和粘贴通过 `insert` 替换选区。默认上限为 64 KiB，调用方可以通过 `with_limit` 指定字节上限。超限时保持草稿和选区。

绘制和定位使用同一次测量的 `FieldLayout`。中文预编辑临时替换选中部分，窗口在提交时写入草稿。鼠标位置先换算成相对于输入区左边的列数，再通过 `layout.byte_at(x, spacing)` 得到字节边界，交给 `field.place(&value, byte, extend)`。按住 Shift 时传入 `true` 扩展选区。窗口负责焦点、输入法启停、单行校验及提交后的业务动作。

[组件入口](../../src/ui/components/grid_text_field/mod.rs) · [文件选择器](../../src/grid/windows/file_picker/view.rs) · [最近项目](../../src/grid/windows/start/projects.rs) · [插件输入](../../src/grid/workspace/plugins/field.rs)
