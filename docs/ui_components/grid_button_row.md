# GridButtonRow

`GridButtonRow<A>` 把本地化的字符按钮和业务动作配对。设置页用它排列五个底部动作，快捷键帮助和插件搜索输入用它绘制 Enter、Esc 按钮，各种确认提示通过 `GridConfirm` 复用同一套测量与命中规则。

```rust
use crate::ui::{components::grid_button_row::GridButtonRow, mnemonic::Mnemonic};
use nanobug::i18n;

let actions = GridButtonRow::new([
    (false, Mnemonic::parse(i18n::text("settings.cancel"))),
    (true, Mnemonic::parse(i18n::text("settings.confirm"))),
]);
let slots = actions.trailing(1..width.saturating_sub(2), footer_rows, 1, spacing);
for slot in &slots {
    frame.button(slot.row, slot, TEXT, spacing);
}
let clicked = GridButtonRow::hit_at(&slots, local_x, local_y);
let keyed = actions.key(&keystroke.key);
```

`slots` 提供单行左对齐或居中布局，`trailing` 把最后一个动作放在右下方，空间不足时把前面的按钮向上换行。两者都返回可见列范围。`hit_at` 接收帧内行列，按钮间隔没有动作。物理键按钮可直接用 `GridButtonRow::enter_escape()`。

调用方决定动作顺序、可用区域以及重复按键的处理方式。译文通过 `&` 指定助记字母，英文如 `&Yes`，中文如 `是(&Y)`。绘制下划线与匹配按键使用同一份解析结果。

[组件入口](../../src/ui/components/grid_button_row/mod.rs) · [设置页](../../src/grid/windows/settings/view.rs) · [搜索输入的按钮](../../src/grid/render/prompt.rs) · [GridConfirm](grid_confirm.md)
