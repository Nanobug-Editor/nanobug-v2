# Nanobug 缺失的 Neovim 0.12.0 Ex 命令

基准为 2026-11-17 核对时的 Nanobug 源码和 Neovim 0.12.0 官方注册表。官方登记 557 个名称，当前有 185 个同名入口，下面列出其余 372 个。统计包含符号命令和上游单独登记的别名，例如 `:xit` 和 `:quitall`。同一条目的自动缩写随条目计数。已存在入口的命令参数缺口，例如 `:edit!` 和 `:wqa!`，另行统计。 [Neovim 官方注册表](https://github.com/neovim/neovim/blob/v0.12.0/src/nvim/ex_cmds.lua) · [Nanobug 命令表](../../src/vim/commands.rs) · [映射分发](../../src/vim/mappings.rs) · [菜单分发](../../src/vim/menu.rs) · [范围编辑](../../src/vim/ex_batch.rs)

## 兼容性判定

| 判定 | 数量 | 依据 |
|---|---:|---|
| 可直接接入 | 97 | 主要行为可复用现有的编辑、缓冲区、窗口、输入或持久化能力 |
| 需新增能力或明确语义 | 128 | 适合本地实现，涉及新的状态或需要确定 Session、Tabpage、窗口与缓冲区的作用范围 |
| 已暂缓 | 110 | Quickfix、Location List、Diff 和源码 tags，按已确认的排期暂缓 |
| 不适合按原义接入 | 37 | 依赖语言宿主、Neovim UI 或运行时包，或针对旧式 GUI 和终端环境 |

“可直接接入”表示存在可复用的本地能力，具体参数和边界仍需验证。“不适合”依据当前 Nanobug 的独立 Rust Ex 范围及现有窗口和插件架构。适配判断是本项目的选择，上游文档用于核对命令原义。 [Nanobug 项目约定](../../AGENTS.md)

## 按用途汇总

| 用途 | 可直接接入 | 需新增能力或明确语义 | 已暂缓 | 不适合按原义接入 | 总计 |
|---|---:|---:|---:|---:|---:|
| 文件打开、命名与工作目录 | 7 | 9 | — | — | 16 |
| 缓冲区与参数列表 | 20 | 13 | — | — | 33 |
| 窗口、Tabpage 与视口 | 7 | 13 | — | — | 20 |
| 文本编辑、定位与替换 | 15 | — | — | — | 15 |
| 包含文件中的标识符与宏 | 10 | — | — | — | 10 |
| 寄存器、标记与历史 | 7 | 9 | — | — | 16 |
| 折叠与拼写 | 9 | 3 | — | — | 12 |
| 退出、重启与模式切换 | 7 | 2 | — | — | 9 |
| 帮助与选项界面 | 6 | 1 | — | — | 7 |
| 界面、本地化、文件类型与语法高亮 | 2 | 13 | — | — | 15 |
| Session、视图、配置导出与恢复 | — | 9 | — | — | 9 |
| Ex 脚本、用户命令与自动命令 | 6 | 29 | — | — | 35 |
| 命令修饰、输出、调试与性能分析 | — | 18 | — | — | 18 |
| 外部工具、语言服务与运行时 | 1 | 5 | — | — | 6 |
| 终端模式映射与菜单 | — | 4 | — | — | 4 |
| Quickfix 与构建、搜索结果 | — | — | 39 | — | 39 |
| Location List | — | — | 39 | — | 39 |
| Diff | — | — | 7 | — | 7 |
| 源码 tags 与标签栈 | — | — | 25 | — | 25 |
| 外部语言解释器入口 | — | — | — | 25 | 25 |
| Neovim 主机协议、旧 GUI 与终端作业控制 | — | — | — | 12 | 12 |
| **合计** | **97** | **128** | **110** | **37** | **372** |

## 逐项清单

### 1. 文件打开、命名与工作目录

现有文件打开和路径解析可承接查找、重命名入口及磁盘状态检查。`view`、`sview` 需要允许编辑同时限制覆盖写盘。工作目录命令需要区分应用、Tabpage、窗口的路径作用域，项目根目录仍用于 Session 和语言服务。`drop` 需要确定焦点切换可跨到哪一层。 [Neovim 文件编辑](https://neovim.io/doc/user/editing/) · [Nanobug 文件跳转与路径解析](../../src/grid/workspace/commands.rs) · [磁盘版本检查](../../src/document/mod.rs#L554)

- **可直接接入（7）**　`:checktime`、`:ex`、`:file`、`:find`、`:pwd`、`:sfind`、`:tabfind`
- **需新增能力或明确语义（9）**　`:cd`、`:chdir`、`:drop`、`:lcd`、`:lchdir`、`:sview`、`:tcd`、`:tchdir`、`:view`

### 2. 缓冲区与参数列表

分屏版本的缓冲区和参数导航可复用现有列表及分屏操作。`badd`、`bunload`、`bwipeout` 和隐藏缓冲区命令涉及独立的缓冲区生命周期。`argglobal`、`arglocal` 需要在 Session 参数列表的基础上支持窗口局部列表。 [Neovim 缓冲区](https://neovim.io/doc/user/windows/) · [Nanobug Session 缓冲区](../../src/grid/workspace/projects/buffers.rs) · [参数列表](../../src/grid/workspace/argument_list.rs)

- **可直接接入（20）**　`:bmodified`、`:brewind`、`:sargument`、`:sbfirst`、`:sblast`、`:sbmodified`、`:sbnext`、`:sbNext`、`:sbprevious`、`:sbrewind`、`:sbuffer`、`:sfirst`、`:slast`、`:snext`、`:sNext`、`:sprevious`、`:srewind`、`:wnext`、`:wNext`、`:wprevious`
- **需新增能力或明确语义（13）**　`:all`、`:argglobal`、`:arglocal`、`:badd`、`:ball`、`:balt`、`:bunload`、`:bwipeout`、`:hide`、`:sall`、`:sball`、`:sunhide`、`:unhide`

### 3. 窗口、Tabpage 与视口

预览、重绘、窗格尺寸和视口定位可复用现有窗口能力。分屏放置修饰符需要传入被修饰命令，`fclose` 需要明确适用的浮动窗口类型，`syncbind` 需要跨窗口同步滚动。 [Neovim 窗口命令](https://neovim.io/doc/user/windows/) · [Tabpage](https://neovim.io/doc/user/tabpage/) · [Nanobug 分屏操作](../../src/grid/workspace/editing_panes.rs)

- **可直接接入（7）**　`:pbuffer`、`:redraw`、`:redrawstatus`、`:redrawtabline`、`:resize`、`:tabrewind`、`:z`
- **需新增能力或明确语义（13）**　`:aboveleft`、`:belowright`、`:botright`、`:browse`、`:confirm`、`:fclose`、`:horizontal`、`:leftabove`、`:rightbelow`、`:syncbind`、`:tab`、`:topleft`、`:vertical`

### 4. 文本编辑、定位与替换

范围内的文本输入、缩进、对齐、去重和替换可以调用本地编辑模型。`iput` 应按目标行调整缩进。`&` 和 `~` 对上次替换参数及搜索模式的取值各有规则。 [Neovim 文本修改](https://neovim.io/doc/user/change/) · [Nanobug 范围编辑](../../src/vim/ex_batch.rs) · [搜索和替换](../../src/vim/pattern.rs)

- **可直接接入（15）**　`:&`、`:<`、`:>`、`:~`、`:append`、`:center`、`:change`、`:goto`、`:insert`、`:iput`、`:left`、`:right`、`:smagic`、`:snomagic`、`:uniq`

### 5. 包含文件中的标识符与宏

这组命令在包含文件里查找标识符或宏，并按入口列出结果、跳转、分屏或预览。Nanobug 已有 Rust 实现的包含文件查询，可以复用。 [Neovim 特殊搜索](https://neovim.io/doc/user/tagsrch/#include-search) · [Nanobug 包含文件查询](../../src/vim/bracket_commands/includes.rs)

- **可直接接入（10）**　`:checkpath`、`:djump`、`:dlist`、`:dsearch`、`:dsplit`、`:ijump`、`:ilist`、`:isearch`、`:isplit`、`:psearch`

### 6. 寄存器、标记与历史

现有寄存器、标记和二合字模型可接收命令入口。撤销时间或保存点、持久撤销和整段消息日志需要新增状态。`rviminfo`、`wviminfo` 是 Neovim 的 ShaDa 别名。完整用途需要扩展目前以编号标记为主的持久化内容。 [Neovim 撤销](https://neovim.io/doc/user/undo/) · [ShaDa](https://neovim.io/doc/user/starting/#shada) · [Nanobug 文档历史](../../src/document/history.rs) · [寄存器](../../src/vim/register.rs)

- **可直接接入（7）**　`:=`、`:@`、`:ascii`、`:delmarks`、`:digraphs`、`:k`、`:mark`
- **需新增能力或明确语义（9）**　`:earlier`、`:later`、`:messages`、`:oldfiles`、`:rundo`、`:rviminfo`、`:undojoin`、`:wundo`、`:wviminfo`

### 7. 折叠与拼写

折叠开关和按折叠状态遍历有现成的文档折叠模型。常见拼写词条操作可复用本地词表。罕见词分级、字典编译和重复上次拼写替换需要新增词典或替换记录。 [Neovim 折叠](https://neovim.io/doc/user/fold/) · [拼写](https://neovim.io/doc/user/spell/) · [Nanobug 折叠模型](../../src/document/folds.rs) · [拼写](../../src/vim/spelling.rs)

- **可直接接入（9）**　`:foldclose`、`:folddoclosed`、`:folddoopen`、`:foldopen`、`:spelldump`、`:spellgood`、`:spellinfo`、`:spellundo`、`:spellwrong`
- **需新增能力或明确语义（3）**　`:mkspell`、`:spellrare`、`:spellrepall`

### 8. 退出、重启与模式切换

`quitall`、`xit` 可接现有退出命令。编辑模式切换和延时可复用现有输入状态。`cquit` 涉及进程退出码，`restart` 需要确定文件修改、Session 保存和重启之间的顺序。 [Neovim 退出](https://neovim.io/doc/user/starting/#exiting) · [`cquit`](https://neovim.io/doc/user/quickfix/#%3Acquit) · [Nanobug 延时](../../src/grid/workspace/sleep.rs) · [退出路由](../../src/grid/workspace/ex_commands.rs)

- **可直接接入（7）**　`:quitall`、`:sleep`、`:startgreplace`、`:startinsert`、`:startreplace`、`:stopinsert`、`:xit`
- **需新增能力或明确语义（2）**　`:cquit`、`:restart`

### 9. 帮助与选项界面

帮助窗口和现有选项可以承接说明、状态与帮助关闭命令。`helptags` 生成帮助主题索引，须与现有本地帮助加载协作。它与源码标签栈属于不同用途。 [Neovim 帮助系统](https://neovim.io/doc/user/helphelp/) · [Nanobug 帮助窗格](../../src/grid/workspace/help_code.rs)

- **可直接接入（6）**　`:exusage`、`:helpclose`、`:intro`、`:options`、`:version`、`:viusage`
- **需新增能力或明确语义（1）**　`:helptags`

### 10. 界面、本地化、文件类型与语法高亮

现有 keymap 解析器和菜单树可用于加载键盘布局、翻译菜单名。Vim 高亮组、语法规则、手动标志和文件类型覆盖须接入 Nanobug 现有主题与语言插件。`tmenu`、`tunmenu` 定义或移除菜单提示文字。 [Neovim 语法与高亮](https://neovim.io/doc/user/syntax/) · [Ex 命令索引](https://github.com/neovim/neovim/blob/v0.12.0/runtime/doc/index.txt) · [Nanobug 菜单](../../src/vim/menu.rs) · [keymap 解析](../../src/vim/keymap.rs)

- **可直接接入（2）**　`:loadkeymap`、`:menutranslate`
- **需新增能力或明确语义（13）**　`:colorscheme`、`:echohl`、`:filetype`、`:highlight`、`:language`、`:match`、`:ownsyntax`、`:setfiletype`、`:sign`、`:syntax`、`:syntime`、`:tmenu`、`:tunmenu`

### 11. Session、视图、配置导出与恢复

`mksession` 导出的是可由 Ex 读回的会话脚本。Nanobug 已有项目 Session 快照，可以复用数据并为导出和加载定义映射。视图文件、配置导出及崩溃恢复各需要明确保存载体和恢复时机。 [Neovim 视图与 Session](https://neovim.io/doc/user/starting/#views-sessions) · [Nanobug 项目快照](../../src/grid/workspace/projects/session.rs) · [全局 Vim 配置](../../src/grid/workspace/vim_config.rs)

- **需新增能力或明确语义（9）**　`:loadview`、`:mkexrc`、`:mksession`、`:mkview`、`:mkvimrc`、`:noswapfile`、`:preserve`、`:recover`、`:swapname`

### 12. Ex 脚本、用户命令与自动命令

本地表达式、函数表和 `:source` 适合扩展求值、输出与脚本退出。控制流、变量锁、自定义用户命令和自动命令分别需要解析、变量作用域、命令登记表和应用事件模型。 [Neovim Vimscript](https://neovim.io/doc/user/vimscript/) · [自动命令](https://neovim.io/doc/user/autocmd/) · [Nanobug Rust 脚本执行器](../../src/vim/script.rs)

- **可直接接入（6）**　`:delfunction`、`:echoerr`、`:echon`、`:eval`、`:finish`、`:scriptnames`
- **需新增能力或明确语义（29）**　`:augroup`、`:autocmd`、`:break`、`:catch`、`:comclear`、`:command`、`:const`、`:continue`、`:defer`、`:delcommand`、`:doautoall`、`:doautocmd`、`:else`、`:elseif`、`:endfor`、`:endif`、`:endtry`、`:endwhile`、`:finally`、`:for`、`:if`、`:lockvar`、`:noautocmd`、`:scriptencoding`、`:throw`、`:try`、`:unlet`、`:unlockvar`、`:while`

### 13. 命令修饰、输出、调试与性能分析

这组需要统一的 Ex 执行上下文来携带状态保留规则、输出捕获、错误和安全约束。断点、单步和性能分析还需要脚本执行点和可观察的统计记录。 [Neovim 命令行](https://neovim.io/doc/user/cmdline/) · [脚本调试](https://neovim.io/doc/user/vimscript/) · [Nanobug 命令分发](../../src/vim/commands.rs)

- **需新增能力或明确语义（18）**　`:breakadd`、`:breakdel`、`:breaklist`、`:debug`、`:debuggreedy`、`:filter`、`:keepalt`、`:keepjumps`、`:keepmarks`、`:keeppatterns`、`:lockmarks`、`:profdel`、`:profile`、`:redir`、`:sandbox`、`:silent`、`:unsilent`、`:verbose`

### 14. 外部工具、语言服务与运行时

显式 `:!` 可接入现有本地 Shell 执行通道。`compiler` 依赖构建结果和诊断适配。`lsp` 和 `checkhealth` 可调用 Nanobug 自身的服务管理和诊断。`runtime` 可面向 Nanobug 或用户提供的脚本路径，`trust` 需要明确哪些本地脚本进入信任记录。 [Neovim LSP](https://neovim.io/doc/user/lsp/) · [健康检查](https://neovim.io/doc/user/health/) · [脚本入口](https://neovim.io/doc/user/vimscript/) · [Nanobug 工作区 Shell 路由](../../src/grid/workspace/commands.rs)

- **可直接接入（1）**　`:!`
- **需新增能力或明确语义（5）**　`:checkhealth`、`:compiler`、`:lsp`、`:runtime`、`:trust`

### 15. 终端模式映射与菜单

这些命令要求终端模式的映射表和输入优先级，需与现有终端字符转发、原生输入及终端模式切换共同实现。 [Neovim 终端](https://neovim.io/doc/user/terminal/) · [Nanobug 终端输入路由](../../src/grid/workspace/terminal.rs)

- **需新增能力或明确语义（4）**　`:tmap`、`:tmapclear`、`:tnoremap`、`:tunmap`

### 16. Quickfix 与构建、搜索结果

需要跨文件结果列表、结果历史、错误位置跳转和工具输出解析。包括将搜索、构建、帮助搜索结果送入该列表的入口。 [Neovim Quickfix](https://neovim.io/doc/user/quickfix/)

- **已暂缓（39）**　`:cabove`、`:caddbuffer`、`:caddexpr`、`:caddfile`、`:cafter`、`:cbefore`、`:cbelow`、`:cbottom`、`:cbuffer`、`:cc`、`:cclose`、`:cdo`、`:cexpr`、`:cfdo`、`:cfile`、`:cfirst`、`:cgetbuffer`、`:cgetexpr`、`:cgetfile`、`:chistory`、`:clast`、`:clist`、`:cnewer`、`:cnext`、`:cNext`、`:cnfile`、`:cNfile`、`:colder`、`:copen`、`:cpfile`、`:cprevious`、`:crewind`、`:cwindow`、`:grep`、`:grepadd`、`:helpgrep`、`:make`、`:vimgrep`、`:vimgrepadd`

### 17. Location List

需要每个编辑窗口持有独立结果列表，并处理窗口复制、关闭和跨文件导航。 [Neovim Location List](https://neovim.io/doc/user/quickfix/#location-list)

- **已暂缓（39）**　`:labove`、`:laddbuffer`、`:laddexpr`、`:laddfile`、`:lafter`、`:lbefore`、`:lbelow`、`:lbottom`、`:lbuffer`、`:lclose`、`:ldo`、`:lexpr`、`:lfdo`、`:lfile`、`:lfirst`、`:lgetbuffer`、`:lgetexpr`、`:lgetfile`、`:lgrep`、`:lgrepadd`、`:lhelpgrep`、`:lhistory`、`:ll`、`:llast`、`:llist`、`:lmake`、`:lnewer`、`:lnext`、`:lNext`、`:lnfile`、`:lNfile`、`:lolder`、`:lopen`、`:lpfile`、`:lprevious`、`:lrewind`、`:lvimgrep`、`:lvimgrepadd`、`:lwindow`

### 18. Diff

需要差异计算、差异块、对齐后的显示和将修改应用到指定缓冲区的能力。 [Neovim Diff](https://neovim.io/doc/user/diff/)

- **已暂缓（7）**　`:diffget`、`:diffoff`、`:diffpatch`、`:diffput`、`:diffsplit`、`:diffthis`、`:diffupdate`

### 19. 源码 tags 与标签栈

需要本地 tags 数据和标签栈。这里含普通、分屏、预览及 Location List 形式。`ltag` 归入 tags，确保只计一次。 [Neovim tags](https://neovim.io/doc/user/tagsrch/)

- **已暂缓（25）**　`:ltag`、`:pop`、`:ppop`、`:ptag`、`:ptfirst`、`:ptjump`、`:ptlast`、`:ptnext`、`:ptNext`、`:ptprevious`、`:ptrewind`、`:ptselect`、`:stag`、`:stjump`、`:stselect`、`:tag`、`:tags`、`:tfirst`、`:tjump`、`:tlast`、`:tnext`、`:tNext`、`:tprevious`、`:trewind`、`:tselect`

### 20. 外部语言解释器入口

这些入口面向 Lua 或其他语言宿主。适配原用途会引入新的解释器或 provider，超出当前本地 Rust Ex 范围。其中若干旧语言入口在上游注册表中也是保留项。 [Neovim 语言 provider](https://neovim.io/doc/user/provider/) · [官方命令表](https://github.com/neovim/neovim/blob/v0.12.0/src/nvim/ex_cmds.lua)

- **不适合按原义接入（25）**　`:lua`、`:luado`、`:luafile`、`:mzfile`、`:mzscheme`、`:perl`、`:perldo`、`:perlfile`、`:py3`、`:py3do`、`:py3file`、`:pydo`、`:pyfile`、`:python`、`:python3`、`:pythonx`、`:pyx`、`:pyxdo`、`:pyxfile`、`:ruby`、`:rubydo`、`:rubyfile`、`:tcl`、`:tcldo`、`:tclfile`

### 21. Neovim 主机协议、旧 GUI 与终端作业控制

`connect`、`detach` 面向 Neovim 的 UI 客户端协议。`packadd`、`packloadall` 面向 Vim 运行时包。Nanobug 已有自己的窗口、插件包和生命周期。其余是旧式 GUI、终端屏幕模式或作业挂起接口，适配为普通桌面窗口操作会改变原有意义。 [Neovim UI 事件](https://neovim.io/doc/user/api-ui-events/) · [运行时包](https://neovim.io/doc/user/repeat/#packages) · [官方 Ex 命令索引](https://github.com/neovim/neovim/blob/v0.12.0/runtime/doc/index.txt) · [Nanobug 源码组织](../使用指南.md#源码组织)

- **不适合按原义接入（12）**　`:connect`、`:detach`、`:gui`、`:gvim`、`:mode`、`:packadd`、`:packloadall`、`:simalt`、`:stop`、`:suspend`、`:winpos`、`:winsize`
