# GridTabBar

编辑区文件标签、工作区项目标签和编号 Tabpage 共用 `GridTabBar`。给它标签数量、每项占用的列数、当前索引和可用列范围，它会返回连续可见的标签、分隔符、两端的溢出计数以及点击范围。

```rust
use crate::ui::components::grid_tab_bar::{GridTabBar, TabAlign, TabGrowth, TabKind, TabOptions};

let bar = GridTabBar::fit(widths.len(), |i| widths[i], active, available, TabOptions {
    gap: spacing.columns(" | "),
    hit_padding: 1,
    minimum: 1,
    growth: TabGrowth::Narrowest,
    align: TabAlign::Start,
});
for slot in &bar.slots {
    if let TabKind::Tab(index) = slot.kind {
        paint_tab(index, slot.columns.clone());
    }
}
let destination = bar.slots.iter()
    .find(|slot| slot.hit.contains(&column))
    .and_then(|slot| slot.target());
```

| 用途 | 适配方式 |
| --- | --- |
| 编辑区文件标签 | `Narrowest` 优先纳入较短的相邻标签。编辑器为图标预留空间，并绘制文件名和修改标记。 |
| 工作区项目标签 | `Retain(first)` 尽量保留原来的首项，`Fill` 将右端溢出箭头放到分配区边缘。工作区绘制选中项动画。 |
| 编号 Tabpage | `LeftFirst` 优先填入左侧相邻页，`End` 将整组对齐到标题栏右侧。 |

`TabKind::Before` 和 `After` 提供隐藏数量和紧邻的目标索引。绘制使用 `columns`，点击使用 `hit`，标签旁的留白按配置计入点击区，分隔符没有目标。窗口负责把索引转换成文件、项目或页的实际动作。

快捷键帮助和插件页签还可用同模块的 `TitleTabs::new(columns, captions, compact, spacing)` 排列固定标题。用 `visible()` 绘制并用 `tab_at(column)` 命中，空标题保留其索引。

[组件入口](../../src/ui/components/grid_tab_bar/mod.rs) · [编辑区](../../src/grid/windows/editor/header.rs) · [项目标题栏](../../src/grid/workspace/tabs.rs) · [编号页](../../src/grid/workspace/tabs/pages.rs)
