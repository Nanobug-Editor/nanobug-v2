# GridTree 网格树

`GridTree` 将一棵已知展开状态的树映射为可见行。绘制、按键移动、光标跟随和鼠标命中共用同一份行顺序。文件管理器和设置页左侧树分别提供文件行、分类行的内容和动作。

## 构建可见行

| 输入 | 构造方式 | 现有调用方 |
| --- | --- | --- |
| 已经按显示顺序排列的 `(节点 ID, 深度)` | `GridTree::from_preorder(TreeRoots::Connected, rows)`。根层深度为 0，每次下探一层。顶层条目也绘制连向项目根的树枝。 | 文件管理器在目录读取成功后，按这次扫描的条目下标构建。 |
| 按稳定节点下标排列的 `(父节点 ID, 已展开)` | `GridTree::from_parents(TreeRoots::Separate, nodes)`。父节点先于子节点，顶层的父节点是 `None`。不同分类的子节点可以交错传入，各分类内部保持插入顺序。 | 设置页在分支展开、收起或插件追加页面时重建可见行。 |

两种构造方式都支持两种根节点样式。`TreeRoots::Separate` 让每个顶层分类独立显示，`TreeRoots::Connected` 为共同的隐含根保留一级连线。节点 ID 由调用方提供。文件管理器在重新扫描后按路径恢复选择，设置页直接沿用稳定节点下标。

## 绘制与点击

```rust
for (y, row) in tree.visible_rows(scroll.first_cell(), viewport.h) {
    let control = row.control_column().min(viewport.w);
    frame.tree_row_guides(y, &row, control);
    // 按 row.id() 取出内容，在 control 放置文件图标或展开箭头。
}

if let Some(hit) = tree.hit(viewport.pixels(metrics), point, metrics.lh, scroll.position()) {
    select_node(hit.id);
}
```

`visible_rows` 返回帧内行号，视口非空时会多提供下一行供平滑滚动裁剪。调用方用同一视口裁剪滚动图层，并传入动画当前的偏移来命中。点击位于视口外或内容末尾下方的空白时返回 `None`。文件管理器可用 `hit.aligned_first` 将视口停在整数行，保持刚才点中的条目仍在鼠标所在行。

按键移动时，用 `position(id)` 查可见行号，再用 `offset(行号, 位移)` 取得目标节点 ID。位移会停在首末行，空树返回 `None`。文件管理器单击选中，双击原路径打开文件或展开目录。设置树单击分类即可折叠或展开，单击叶节点切换设置页面。

文件系统扫描、分支的展开状态、选择、焦点和滚动动画都由窗口持有。[文件管理器](../../src/grid/windows/manager/tree.rs) · [设置页](../../src/grid/windows/settings/mod.rs) · [滚动与网格布局](../光标与滚动动效.md#滚动与网格布局)
