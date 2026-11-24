# GridConfirm 操作确认

`GridConfirm<A>` 把每个可见选项与动作 `A` 绑定。关闭未保存缓冲区、从最近列表移除项目、文件选择器删除文件、插件安装审阅和卸载确认都用它。文件选择器把确认行画在底部，其余调用方可把按钮放在独立对话框或插件页面里。

## 定义选项

动作可以是布尔值，也可以是实现了 `Copy` 的枚举。翻译文件用 `&` 标记助记键，例如英文 `&Yes`、中文 `是(&Y)`。按键匹配忽略大小写，绘制时为解析出的字母加下划线。

```rust
use crate::ui::{
    components::grid_confirm::{ButtonAlign, GridConfirm},
    mnemonic::Mnemonic,
};
use nanobug::i18n;

let choices = GridConfirm::new([
    (true, Mnemonic::parse(i18n::text("start.remove_yes"))),
    (false, Mnemonic::parse(i18n::text("start.remove_no"))),
]);
let slots = choices.slots(2..width.saturating_sub(2), ButtonAlign::Center, 2, spacing);
for button in &slots {
    frame.button(row, button, ink, spacing);
}
```

`slots` 接收帧内的列范围、对齐方式和按钮间隔，返回绘制顺序及裁剪后的可点击范围。`ButtonAlign::Start` 适合底部内嵌行。键盘用 `choices.key(&keystroke.key)`，鼠标先判断按钮行，再把帧内列传给 `GridConfirm::hit(&slots, column)`。两者返回同一个业务动作，点击间隔返回 `None`。

按钮的测量、绘制槽位和命中由 [GridButtonRow](grid_button_row.md) 提供。插件提示使用 `GridConfirm::enter_escape()`，`Enter` 接受、`Esc` 取消。调用方负责过滤长按、修饰键和重复点击，决定取消或点击外部的行为，并执行保存、移除或卸载。

[组件入口](../../src/ui/components/grid_confirm/mod.rs) · [关闭编辑区](../../src/grid/workspace/editor_close.rs) · [最近项目](../../src/grid/windows/start/remove_project.rs) · [文件删除](../../src/grid/windows/file_picker/view.rs) · [插件确认](../../src/grid/workspace/plugins/manager_input.rs)
