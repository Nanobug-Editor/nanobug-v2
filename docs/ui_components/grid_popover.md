# GridPopover 锚定浮层

`GridPopover` 把内容放到光标或控件附近。普通模式的 `K` 定义详情、插入模式的补全候选和设置里的单选、多选下拉列表共用这套定位规则。

## 光标旁的浮层

以裁剪后的编辑区正文作为 `body`。正文范围、光标和最终绘制矩形都使用窗口内的逻辑像素。光标行完整可见时才创建浮层。

```rust
use crate::ui::components::grid_popover::GridPopover;
use gpui::size;

let popover = GridPopover::at_caret(body, caret, row_height)?;
let height = desired_height.min(gpui::px(popover.available_height()));
let bounds = popover.pixel_bounds(body, caret.x, size(width, height));
```

`available_height()` 返回光标上、下两侧中较大的可用高度。调用方据此限制正文行数，并为标题、边缘、页脚预留空间。`pixel_bounds` 优先在下方显示，下方不足时翻到上方，并把横向范围限制在正文内。绘制、链接、候选项点击与滚轮命中使用同一份 `bounds`。

## 设置下拉列表

以表单视口为基准，把滚动后的字段行传给 `GridPopover::new(0.0..body.h as f32, anchor..anchor + 1.0)`。字段至少一部分可见时，`rows(count, allow_border)` 返回首行 `top`、整行高度 `height` 和每端的边框行数 `inset`。保留 `top` 的小数部分，边框、选项、光标和点击就会随表单平滑滚动。两侧都不足一整行时，在字段位置显示一个选项。空选项列表由调用方直接关闭。

定义详情保留 Markdown、链接和滚动状态，补全菜单保留候选选择，设置表单保留草稿和多选状态。

[组件入口](../../src/ui/components/grid_popover/mod.rs) · [定义详情](../../src/grid/windows/editor/hover.rs) · [插入补全](../../src/grid/windows/editor/completion.rs) · [设置下拉列表](../../src/grid/windows/settings/selector.rs)
