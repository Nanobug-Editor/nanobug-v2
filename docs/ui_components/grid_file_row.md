# GridFileRow 文件行

`GridFileRow` 在一行字符网格内绘制文件或目录。文件选择器的父目录、当前目录和目录预览，以及启动页的项目预览，都使用默认列布局。文件管理器把名称列对齐到树的缩进位置。

## 绘制一行

```rust
use crate::ui::components::grid_file_row::{FileRowState, GridFileRow};

let mut file = GridFileRow::new(&path, is_directory, expanded, left..right);
if selected {
    file.state = FileRowState::selection(active);
}
file.matched = matches_query;
file.mark = marked.then_some(left + 1);
frame.file_row(y, &file, spacing, None);
```

`left..right` 和 `y` 都是当前帧内的网格坐标，右端不包含在内。整行背景覆盖这段列范围。默认图标在 `left + 2`，文件名从 `left + 4` 开始。图标按路径和目录展开状态匹配，名称按实际字宽截断并显示 `…`。卷根使用可读路径作为名称，按名称过滤时可复用 `file_name(path)`。

| 显示需求 | 调用方式 |
| --- | --- |
| 普通预览 | 保留默认样式，直接调用 `frame.file_row`。 |
| 当前选择或后台保留的选择 | 传入 `FileRowState::selection(true)` 或 `FileRowState::selection(false)`。 |
| 在目录树内指示正在编辑的文件 | 将 `state` 设为 `FileRowState::ActiveFile`。 |
| 搜索命中或批量标记 | `matched` 将名称设为搜索匹配色，`mark` 指定绘制圆点的列。两者可以同时生效。 |

## 树缩进与长名称

在绘制前设置 `file.name_columns = start..end` 可以改变名称槽。图标随名称移动，树枝使用 `file.icon_column()` 作为终点。

文件管理器允许当前行的长名称延伸到相邻窗格。它向 `frame.file_row` 传入 `Some(可用列数)`。如果名称在原槽内放不下且外侧更宽，方法会画好行背景、标记和图标，然后返回按外侧宽度截断的名称。调用方用 `file.ink()` 在独立图层绘制这段名称。传入 `None` 时名称始终只在本行绘制。

窗口持有目录数据、搜索、选择、标记及点击动作。[文件管理器](../../src/grid/windows/manager/view.rs) · [文件选择器](../../src/grid/windows/file_picker/view.rs) · [启动页项目预览](../../src/grid/windows/start/projects.rs)
