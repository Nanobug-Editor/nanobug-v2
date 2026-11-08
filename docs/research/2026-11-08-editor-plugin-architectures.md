# 编辑器插件架构一手资料研究

日期：2026-11-08。范围：Nanobug V2 的启动、插件按需加载、绘制通信、文档一致性、恢复和接口演进。本文是研究笔记和待讨论建议，不是已冻结规格；没有运行性能测试，也没有实现插件系统。

## 研究前提

现有需求明确要求每个已激活插件一个 runtime 进程，包内模块分别按需加载，插件自主实现区域内部界面逻辑，JS 只负责启用条件。这些约束来自[需求总结](C:/Projects/RustProjects/nanobug/docs/需求总结.md)，不是从其他编辑器推导出的决定。

当前编辑核心是自研 Rust 文档模型和简易 Vim 状态机；`Cargo.toml` 中“embedded Neovim core”描述与当前源码不一致。研究 Neovim 仅为借鉴协议，不意味着引入它。[main.rs](C:/Projects/RustProjects/nanobug/src/main.rs:1)、[lib.rs](C:/Projects/RustProjects/nanobug/src/lib.rs:1)、[vim.rs](C:/Projects/RustProjects/nanobug/src/editor/vim.rs:1)、[使用指南](C:/Projects/RustProjects/nanobug/docs/使用指南.md:76)

## VS Code：借鉴激活条件和可观测性

**已核实事实。** VS Code 通过清单中的 activation events 在命令调用、语言文件打开、视图打开、工作区条件等事件发生时激活扩展。官方建议只在用户需要时激活，尽量避免启动即激活的 `*`；`onStartupFinished` 是启动后触发。清单声明不等于执行插件主入口。[Activation Events](https://code.visualstudio.com/api/references/activation-events)

**对 Nanobug 的建议及后续修正。** 安装或更新时生成声明式索引：命令、语言、视图入口、资源类型和规则依赖。启动时读取小型索引展示入口，JS 根据已缓存上下文判断模块条件。用户第二轮明确条件满足即放行，并否决闲置回收，因此撤回此前“规则允许后还需实际使用才激活”的建议。保留模块加载完成与功能就绪的区分，不照搬 VS Code 的激活语义覆盖用户配置。

**取舍。** 延迟加载会将进程启动、模块加载和初始化成本转移到后续触发时刻，不能只测主窗口启动。按 Nanobug 最新条件驱动规则，应记录条件首次匹配到模块就绪，以及用户打开面板/请求补全到结果可用的时间；编译和执行调度遵守预算，不据使用预测运行配置未选中的模块。这里是机制推断，不是性能实测结果。

**已核实事实。** VS Code 的 extension host 依据本地、Web、远程等运行位置选择 Node.js 或浏览器 Worker；这些机制并不承诺每个插件独占一个进程。其官方性能排查方法包括禁用扩展、扩展二分排查、运行中扩展 CPU profile、启动 profile 和 Startup Performance 时间线。[Extension Host](https://code.visualstudio.com/api/advanced-topics/extension-host)、[Performance Issues](https://github.com/microsoft/vscode/wiki/Performance-Issues)

**对 Nanobug 的建议。** 保留现有“一插件一进程”需求，借鉴按插件归因的诊断能力：记录激活原因、初始化阶段耗时、任务排队、CPU、内存、IPC 流量、超时和重启。首屏之后仍可能因后台加载争用 CPU 而卡顿，因此启动统计应覆盖进入可编辑状态后的交互窗口。单纯显示进程名不足以定位慢模块。

## Zed：借鉴资源与执行逻辑分离，不能推导任意自绘能力

**已核实事实。** Zed 的扩展使用 `extension.toml`；过程逻辑以 Rust 编写并编译为 WebAssembly，当前文档使用 `wasm32-wasip2`。多数资源扩展无需 Rust 代码。官方列出的扩展类型是语言、调试器、主题、图标主题、代码片段、MCP；这份能力表并没有提供 Nanobug 所需任意 DrawList 界面的证据。[Developing Extensions](https://zed.dev/docs/extensions/developing-extensions)

**已核实事实。** Zed 的语言扩展可以声明语言服务器，并通过 Rust 的 `language_server_command` 返回程序、参数和环境。WASM 扩展逻辑与其启动的原生语言服务器是不同执行对象。其 capability 设置可限制进程执行、下载、npm 安装；权限不足时调用返回错误。[Language Extensions](https://zed.dev/docs/extensions/languages)、[Extension Capabilities](https://zed.dev/docs/extensions/capabilities)

**对 Nanobug 的建议。** 把主题、查询、图片等作为直接可读取资源；执行模块只有确实需要程序逻辑时才加载。grammar 与业务模块分别采用各自加载接口。原生 LSP/工具由受管进程接口启动、退出和诊断，不能因为外层是跨平台 WASM 包，就宣称包中所有工具都天然跨平台。DrawList、输入、IME、字体测量和资源生命周期仍需 Nanobug 自行设计。

**已核实事实。** Zed 扩展接口 README 明确说明：以较新接口构建的扩展不能假定兼容旧版 Zed，并维护接口与宿主版本的对应表。[Extension API README](https://github.com/zed-industries/zed/blob/main/crates/extension_api/README.md)

**对 Nanobug 的建议。** 分开记录插件包版本、清单格式版本、宿主接口兼容范围和可选能力集合。宿主升级时，应在实际运行插件代码前检测兼容性；能够降级的可选能力不使整个插件失效。用户第二轮已确定默认兼容前两个旧接口版本，以 N/N−1/N−2 表示；需要验证这些版本的行为兼容，不能只检查一个 `version` 字段。正式编号粒度、迁移通知与发布节奏仍待定。

## Neovim：借鉴版本化增量与整批呈现

**已核实事实。** `nvim_buf_attach` 可订阅初始文档与行级变更，事件含 `changedtick`；`more` 表示一次修改分成多个通知，`changedtick = null` 还可能代表预览显示而非真实文档修改。重载可能导致 detach，需要重新同步。[Buffer update events](https://neovim.io/doc/user/api/#api-buffer-updates)

**对 Nanobug 的建议。** 自研 `Document` 已有 `revision`，可作为版本化文档接口的起点。宿主拥有权威内容；插件接收带文档标识、会话代次、基础版本和新版本的快照/增量。插件返回诊断、高亮或编辑时附带所依据版本，宿主按结果种类拒绝、重算或明确变换过时结果。重连时先同步快照，不能让新进程接着使用旧进程留下的句柄。[Document](C:/Projects/RustProjects/nanobug/src/editor/document.rs:34)

这项建议不要求复制 Neovim 事件结构。尤其不能把 `Document.revision` 存在等同于跨进程增量协议已经完成；还须确定多修改事务、撤销、文档重开、字节/字符坐标及丢失增量后的恢复语义。

**已核实事实。** Neovim 的 UI 更新通过 `redraw` 批次发送；事件按顺序应用，只有收到 `flush` 后才呈现完整状态。一个完整画面可能跨多个批次。`grid_line` 用连续行段、样式编号和重复计数减少表示成本。该协议也允许以后增加事件和参数。[Nvim UI protocol](https://neovim.io/doc/user/api-ui-events/)

**对 Nanobug 的建议。** 插件按视图提交完整绘制批次或明确版本基础的增量；宿主验证并完成整批后才切换显示状态，复用未变化的内容。每帧逐字符同步往返会放大通信次数，应优先批量提交行段、绘图原语和资源句柄。必须区分两种队列：完整可替代快照可保留最新一份；依赖前序状态的增量不能任意丢掉，中途丢弃后必须重同步。

**已核实事实。** Neovim 的兼容约定允许增加可选字段和事件；发布后的事件参数不删除、不重排，私有接口不受相同承诺保护。[API contract](https://neovim.io/doc/user/api/#api-contract)

**对 Nanobug 的建议。** 稳定接口和实验绘图能力分开版本化，保留明确的能力协商。可忽略的未知字段与必须理解的操作码必须区分，不能把“忽略未知数据”应用到会改变资源生命周期或状态顺序的消息。

## 恢复：重启进程只是开始

**已核实事实。** VS Code 当前源码在 extension host 崩溃后记录信息，并依据 crash tracker 决定自动重启或显示人工恢复入口；重复崩溃时提供重启、二分排查等操作。该代码证明有重启限流机制，不证明所有插件状态可透明恢复。[nativeExtensionService.ts](https://github.com/microsoft/vscode/blob/main/src/vs/workbench/services/extensions/electron-browser/nativeExtensionService.ts)

**对 Nanobug 的建议。** 在每插件一进程的基础上增加监督状态机：异常退出后使旧请求和资源句柄失效，回收资源，有限次数退避重启，连续失败时停止自动循环并保留手动重试入口。UI 可保留最后有效画面并显示失效状态，但旧画面不应继续接受会产生操作的输入。恢复后从宿主快照重建，并让插件声明哪些状态可恢复。

**副作用语义。** 查询与可重建计算可重新提交；写文件、执行命令、发送网络请求等操作不能一律在重启后重放。执行完成但响应丢失时，调用方无法单凭“没收到成功”判定操作未发生。建议使用宿主管理的操作 ID、提交记录或具体接口的幂等条件；无法核实的操作标记为结果未知，由用户或上层流程判断。这是分进程重试的设计推论，不是 VS Code 对任意插件提供的保证。

## LSP：取消、版本和兼容能力可以借鉴，协议不等于强制终止

**已核实事实。** LSP 的取消是通知，不保证接收方立即停止；被取消的请求仍需要结束并返回响应。双方在初始化时交换能力，而不是仅凭版本号假定所有功能存在。[LSP 3.17 协议源文档](https://github.com/microsoft/language-server-protocol/blob/gh-pages/_specifications/lsp/3.17/specification.md)

**已核实事实。** 文档变更含修改后版本；增量内容必须按通知顺序及数组顺序应用。诊断版本字段可以是可选的，不能假定所有响应都带文档版本。[LSP 3.17 meta model：DidChangeTextDocumentParams / PublishDiagnosticsParams](https://github.com/microsoft/language-server-protocol/blob/gh-pages/_specifications/lsp/3.17/metaModel/metaModel.json)

**对 Nanobug 的建议。** 内部插件协议统一携带请求 ID、运行代次和输入快照版本，超时后不再采用过期结果，同时独立处理取消、计算配额和必要时终止进程。接入原生 LSP 时由适配器保留请求发起时的文档版本，按各类结果设计失效规则；未携带版本的推送诊断只能采取明确的保守策略，不能伪造精确版本。

## 本轮应交给用户选择的事项

以下属于决策，外部资料不能替用户回答：

1. 启动可编辑、首次插件交互和稳定输入分别采用什么体验目标，以及在哪类参考机器和工作区上验收。
2. 高可用是否要求插件崩溃后编辑器继续可用、插件自动恢复；插件面板尚未提交的临时输入是否也必须无损恢复。
3. 绘图第一版必须覆盖的原语，以及是否包含任意着色器、外部纹理等高成本能力。
4. 用户现已确定兼容前两个旧插件接口版本；仍需明确接口版本编号粒度、淘汰通知与发布节奏，以及实验能力是否允许更快演进。

## 推荐验证场景（尚未执行）

| 场景 | 应观测的结果 |
| --- | --- |
| 已安装很多插件，但当前文件只需少量模块 | 首屏是否依赖无关插件，实际启动了多少进程/模块 |
| 首次打开某插件与再次打开 | 分开报告进程、模块加载、首次有效绘制及后续交互延迟 |
| 一个插件索引任务长时间占用 CPU | 编辑输入和该插件 UI 是否仍能获得调度，宿主是否有明显帧延迟 |
| 插件生成大量旧绘制结果或诊断 | 队列是否有界，是否出现旧内容覆盖新内容 |
| 杀死插件 runtime 后恢复 | 文档、版本、句柄、面板状态是否一致；是否形成重启循环 |
| 写操作完成后、回复前崩溃 | 是否避免重复副作用，能否区分失败与结果未知 |
| 新宿主加载旧插件、旧宿主加载新插件 | 是否在加载前报告兼容性，支持的能力是否按约定降级 |

这些场景来自上述机制分析，不代表其他编辑器或 Nanobug 已通过验收。阈值、资源配额和恢复时限需要讨论后通过实测确定。
