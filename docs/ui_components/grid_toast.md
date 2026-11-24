# GridToast 临时消息

文件选择器用 `GridToast` 显示文件操作的结果、警告和错误。消息浮在预览区右上方，两秒后自动消失。显示期间可以继续操作下面的窗口。

## 显示与替换

在拥有消息的窗口状态里保存一个 `Option<GridToast>`。出现下一条时直接替换该字段，每条消息都有自己的两秒计时。空白文本会清除当前消息。下面的代码假设调用方将字段命名为 `toast`。

```rust
use crate::ui::components::grid_toast::{GridToast, MessageLevel};

self.toast = GridToast::show(MessageLevel::Warning, message, window, cx, |view, cx| {
    view.toast = None;
    cx.notify();
});
cx.notify();
```

`MessageLevel::Info` 显示普通反馈，`Warning` 和 `Error` 使用各自的轮廓颜色、图标和本地化标题。组件将换行和制表符替换为空格，按实际字符宽度排版，正文最多三行、每行最多 54 列，放不下的末行用省略号收尾。

## 绘制

```rust
use crate::grid::render::Frame;

let (area, frame) = Frame::toast(toast, viewport, available_columns, spacing);
```

`viewport` 是所属窗口的网格矩形，`available_columns` 是允许占用的总列数。调用方安装 `frame`，用 `area` 裁剪下层内容，并自行确定与交互弹窗的叠放顺序。消息随所属窗口一起销毁，替换或销毁都会取消旧计时器。

[组件入口](../../src/ui/components/grid_toast/mod.rs) · [文件选择器反馈](../../src/grid/workspace/file_picker.rs) · [网格帧绘制](../../src/grid/render/components.rs)
