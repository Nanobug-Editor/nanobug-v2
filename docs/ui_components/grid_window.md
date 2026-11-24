# GridWindow 网格窗口

`GridWindow` 给带一格边框的窗格划分四个区域。标题占上边框，固定顶部行在标题下方，固定底部行在下边框上方，中间的正文视口可以独立滚动。所有矩形沿用传入外框的字符网格坐标。

## 使用

用外框矩形与固定行数构造布局。编辑区使用 `GridWindow::new(area, 0, 0)`，文件标签直接画在标题行。文件管理器使用 `GridWindow::new(area, 1, 0)`，把项目根路径放在第一行固定内容里。

```rust
use crate::grid::render::Frame;
use crate::ui::{components::grid_window::GridWindow, theme::ThemeComponent};

let window = GridWindow::new(area, 1, 0);
let chrome = Frame::grid_window(window, ThemeComponent::Explorer, selected, |frame, columns, ink| {
    Some(frame.label(0, columns, nanobug::i18n::text("manager.title"), ink, spacing))
});
let body = window.body();
```

| 区域 | 调用方用途 |
| --- | --- |
| `window.bounds()` | 放置固定窗框层。 |
| `window.title()` | 绘制标题和判断标题点击。外框宽或高不足两格时标题区域为空。标题回调接收帧内列范围和焦点色，返回标题右侧的开区间端点。无可见标题时返回 `None`，上边框连续。 |
| `window.header()`、`window.footer()` | 放置始终可见的行。画进窗框帧时，将坐标减去 `window.bounds()` 的原点。 |
| `window.body()` | 提供正文绘制、滚动裁剪、鼠标命中和光标定位的同一块视口。空间不足时先分配边框和顶部固定行，再分配底部行，正文高度可为零。 |

网格渲染器的 `Frame::grid_window` 设置窗框主题，给标题留出边框缺口，并让正文区域由独立内容层着色。窗口自行排布文件标签、行号或目录项，并持有自己的滚动与选择状态。工作区负责窗格分屏、焦点及相邻边框的像素偏移。

单标题的 `Panel::title_at` 默认覆盖文字和左右各一格留白。多页签窗口用 [GridTabBar](grid_tab_bar.md) 模块的 `TitleTabs` 测量可见标题，通过 `visible()` 绘制，再用 `tab_at(column)` 或 `hit_columns(index)` 处理各自的点击。页签之间的 `|` 保留为分隔符。紧凑布局用裸 `|` 时，只有实际存在的留白属于点击范围。

组件入口在 [grid_window/mod.rs](../../src/ui/components/grid_window/mod.rs)。实际接入见[编辑区](../../src/grid/windows/editor/view.rs)和[文件管理器](../../src/grid/windows/manager/view.rs)。
