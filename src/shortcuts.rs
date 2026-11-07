//! Shortcut rows displayed in the focused pane's help overlay.

/// Bindings available in both panes. Printable global commands use Vim navigation modes.
pub const GLOBAL: &[(&str, &str)] = &[
    ("Tab", "切换区域，恢复已关闭的窗格"),
    ("X / qq", "关闭当前窗格"),
    ("?", "打开当前窗格的快捷键帮助"),
    ("F5", "刷新项目文件树"),
    ("F6 / Shift+F6", "正向 / 反向切换光标动效"),
    ("F7", "重载项目光标配置"),
];

/// Bindings specific to the file manager.
pub const MANAGER: &[(&str, &str)] = &[
    ("j / k  ↓ / ↑", "向下 / 向上移动，可加数字次数"),
    ("h / ←", "折叠当前目录或上一级目录"),
    ("l / Enter", "打开文件或展开 / 折叠目录"),
    ("→", "展开目录"),
    ("gg / G  Home / End", "跳到第一项 / 最后一项"),
    ("zz / zt / zb", "当前项居中 / 置顶 / 置底"),
    ("s / Esc", "标记或取消当前项 / 清除标记"),
    ("c / x", "复制 / 剪切所有标记项"),
    ("v", "粘贴到当前目录"),
    ("z / Z", "撤销 / 重做粘贴，保留最近 10 条"),
];

/// Bindings specific to the editor, grouped by operation and input mode.
pub const EDITOR: &[(&str, &str)] = &[
    ("h j k l  方向键", "左右上下移动，支持数字次数"),
    ("Shift+滚轮", "左右滚动正文"),
    ("w b e  0 ^ $", "按词移动，跳到行首、首个非空字符或行尾"),
    ("Home / End", "行首 / 行尾"),
    ("gg / G / 3G", "首行 / 末行 / 指定行"),
    ("PageUp / PageDown", "向上 / 向下翻页"),
    ("i a I A o O", "在当前位置、行首尾或新行进入插入模式"),
    ("Esc / Ctrl+[", "回到普通模式"),
    ("Backspace / Delete", "插入模式下向前 / 向后删除"),
    ("Enter / Tab", "插入模式下换行 / 插入制表符"),
    ("Ctrl+w / Ctrl+u", "插入模式下删除前一词 / 删除到行首"),
    ("Ctrl+v", "在插入、搜索和命令行中粘贴系统剪贴板"),
    ("x s D C", "删除字符、替换字符、删除或修改到行尾"),
    ("d c y + 移动", "删除 / 修改 / 复制移动范围"),
    ("dd cc yy", "整行删除 / 修改 / 复制，支持次数"),
    ("p / P", "在光标后 / 前粘贴"),
    ("v / V", "进入字符 / 行选择，使用 d x c y 操作"),
    ("u / Ctrl+r", "撤销 / 重做文本修改"),
    ("J", "合并下一行"),
    ("/文本  Enter", "字面搜索，使用 n / N 查找下一处 / 上一处"),
    (":w / Ctrl+s", "保存，可用 :w 文件名另存"),
    (":e 文件名", "打开文件"),
    (":q  :q!  :wq  :x", "退出、放弃修改退出或保存退出"),
];
