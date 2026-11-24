# GridPopup 临时弹窗

按下 `<leader>` 后出现的下一键提示使用 `GridPopup`。Vim 前缀、自定义映射和拼写候选复用同一提示窗口。Vim `:popup` 菜单和文件选择器的搜索、过滤输入框也使用这个组件。

## 选择位置

传入可用的字符网格 `viewport`，把固定的标签、状态栏和命令历史区域预先扣除。以下写法把宽 34 列、高 10 行的提示放在右下角。

```rust
use crate::grid::render::Frame;
use crate::ui::components::grid_popup::GridPopup;

let area = GridPopup::new(viewport).bottom_right(34, 10);
let frame = Frame::titled(area, title, spacing);
```

| 入口 | 适用位置 |
| --- | --- |
| `bottom_right(width, height)` | 按键序列提示，对齐剩余工作区的右下角。 |
| `at(rect)` | Vim 菜单，优先采用鼠标或光标给出的起点。 |
| `centered_on_row(row, width, height)` | 搜索和过滤框，水平居中并尽量贴近指定行。 |

所有入口均返回工作区坐标，先限制宽高，再把矩形平移到视口内。窗口用同一个矩形放置正文、裁剪背景和判断点击。提示延时、翻页、选择菜单项或输入法组合由调用方处理。例如搜索框打开时接受文本输入，按键序列提示继续把下一键交给原有的按键分派。

[组件入口](../../src/ui/components/grid_popup/mod.rs) · [按键序列提示](../../src/grid/workspace/leader_hint.rs) · [Vim 菜单](../../src/grid/windows/vim_menu.rs) · [文件选择器搜索和过滤](../../src/grid/windows/file_picker/view.rs)
