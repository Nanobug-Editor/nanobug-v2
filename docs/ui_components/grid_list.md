# GridList

最近项目、插件管理器、设置下拉选项和编辑器补全使用 `GridList` 布局等高条目。它计算可见索引、边缘的半行和指针下的条目。行的文字、图标、选中背景与激活动作由各窗口提供。

```rust
use crate::ui::components::grid_list::GridList;

GridList::follow(&mut scroll, items.len(), viewport.h, selected, now);
let list = GridList::scrolled(items.len(), viewport.h, scroll.position());
for (local_row, index) in list.range().enumerate() {
    draw_item(local_row, &items[index]);
}
let clicked = list.hit(pointer_y_in_rows);
```

`scroll` 是由窗口持有的 `GridScroll`，`now` 使用工作区动画时钟。通过工作区逐帧推进当前偏移，按 `scroll.fraction()` 平移条目层并裁剪在列表矩形内。标题、输入框和页脚固定。点击先检查自己的视口，再将指针换算为距离视口顶部的浮点行数，保留小数部分。设置下拉还需计入锚点随表单滚动产生的位移。

需要立即测量选中项的可见范围时，用 `GridList::reveal(count, height, first, selected)`。已有滚动模型时，直接传可视位置给 `scrolled`。过滤、选择值、补全排序和键盘首尾行为由调用方决定。

[组件入口](../../src/ui/components/grid_list/mod.rs) · [最近项目](../../src/grid/windows/start/projects.rs) · [插件管理器](../../src/grid/windows/plugin_manager/view.rs) · [设置下拉](../../src/grid/windows/settings/selector.rs) · [补全候选](../../src/grid/windows/editor/completion.rs)
