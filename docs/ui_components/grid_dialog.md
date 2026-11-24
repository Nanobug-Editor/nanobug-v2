# GridDialog 对话框

快捷键帮助、设置、界面语言选择和插件管理器用 `GridDialog` 布局。按 `<leader>?` 打开帮助后，方向键浏览帮助内容，`Esc` 返回原窗口。插件管理器内的详情、安装审阅、文档大纲和语言服务结果沿用插件窗口的对话框布局。

## 放置和绘制

`viewport` 是对话框可以占用的字符网格矩形。先扣除需要一直可见的状态栏或标签行，然后把返回的 `area` 同时交给绘制、滚动裁剪和鼠标命中。

```rust
use crate::grid::render::Frame;
use crate::ui::components::grid_dialog::GridDialog;

let dialog = GridDialog::new(viewport);
let area = dialog.centered(72, 24);
let frame = Frame::titled(area, title, spacing);
```

- `centered(width, height)` 在视口内水平、垂直居中。宽高超过视口时按可用格数裁剪。插件列表和详情在 `ManagerWindow::area` 内统一限制最大宽高，再用它居中。
- `centered_on_row(row, width, height)` 水平居中，顶部尽量靠近给定的工作区行。
- `bottom_right(width, height)` 对齐视口右下角。语言选择器用它放在状态栏上方。

简单标题用 `Frame::titled`。帮助窗口的标题包含标签和关闭按钮，用 `Frame::with_title` 在可用列范围内绘制，并返回已占用的右边界。帮助和插件管理器使用可选的 `DialogBackdrop` 缓存背景。工作区提供可见图层，在主题或 DPI 变化时使缓存失效，在对话框关闭时释放图像。焦点、文本输入、滚动和关闭动作由相应窗口处理。

对话框内有接收焦点的子窗口时，外框用 `Frame::with_title` 的普通边框色，子窗口用焦点色。插件浏览器的列表边框是这一用法，描述框使用说明文字的边框色。

[组件入口](../../src/ui/components/grid_dialog/mod.rs) · [快捷键帮助](../../src/grid/windows/help/view.rs) · [设置](../../src/grid/windows/settings/view.rs) · [语言选择](../../src/grid/workspace/language.rs) · [插件管理器](../../src/grid/workspace/plugins/presentation.rs)
