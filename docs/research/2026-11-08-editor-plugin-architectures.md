# 编辑器插件架构一手资料研究

日期：2026-11-08。范围：Nanobug V2 的启动、插件按需加载、绘制通信、文档一致性、恢复和接口演进。本文是研究笔记和待讨论建议，不是已冻结规格；没有运行性能测试，也没有实现插件系统。

2026-11-09 后续决定：用户已明确采用 VS Code 的版本兼容与实验接口规范。当前方案见[专项设计](2026-11-09-plugin-api-versioning.md)；文中的早期版本窗口建议仅保留为历史，不再作为当前淘汰规则。

## 研究前提

现有需求明确要求每个已激活插件一个 runtime 进程，包内模块分别按需加载，插件自主实现区域内部界面逻辑，JS 只负责启用条件。这些约束来自[需求总结](C:/Projects/RustProjects/nanobug/docs/需求总结.md)，不是从其他编辑器推导出的决定。

当前编辑核心是自研 Rust 文档模型和简易 Vim 状态机；`Cargo.toml` 中“embedded Neovim core”描述与当前源码不一致。研究 Neovim 仅为借鉴协议，不意味着引入它。[main.rs](C:/Projects/RustProjects/nanobug/src/main.rs)、[lib.rs](C:/Projects/RustProjects/nanobug/src/lib.rs)、[vim.rs](C:/Projects/RustProjects/nanobug/src/vim/mod.rs)、[使用指南](C:/Projects/RustProjects/nanobug/docs/使用指南.md:76)

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

**对 Nanobug 的建议及后续修正。** 分开记录插件包版本、清单格式版本、宿主兼容范围和可选能力集合。宿主升级时，应在实际运行插件代码前检测兼容性；能够降级的可选能力不使整个插件失效。早先 N/N−1/N−2 窗口已由用户最新选择的 VS Code 模式替代：稳定接口长期兼容，实验接口独立演进；需要验证历史产物的类型与行为，不能只检查一个 `version` 字段。

## Neovim：借鉴版本化增量与整批呈现

**已核实事实。** `nvim_buf_attach` 可订阅初始文档与行级变更，事件含 `changedtick`；`more` 表示一次修改分成多个通知，`changedtick = null` 还可能代表预览显示而非真实文档修改。重载可能导致 detach，需要重新同步。[Buffer update events](https://neovim.io/doc/user/api/#api-buffer-updates)

**对 Nanobug 的建议。** 自研 `Document` 已有 `revision`，可作为版本化文档接口的起点。宿主拥有权威内容；插件接收带文档标识、会话代次、基础版本和新版本的快照/增量。插件返回诊断、高亮或编辑时附带所依据版本，宿主按结果种类拒绝、重算或明确变换过时结果。重连时先同步快照，不能让新进程接着使用旧进程留下的句柄。[Document](C:/Projects/RustProjects/nanobug/src/document/mod.rs)

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
4. 版本治理后续已改为 VS Code 模式；Q18 已确认基础接口优先稳定、UI SDK 与自定义绘图先实验交付，具体函数清单及 SDK/协议格式仍待设计，见版本专项设计。

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

## 2026-11-09 补充：VS Code 如何处理插件接口版本

本节核对了当日官方文档、`microsoft/vscode` 与 `microsoft/vscode-vsce` 源码；没有修改实现或运行兼容性测试。最初研究时未改变 Nanobug 决策；其后用户已明确采用该治理方向，当前规则见[专项设计](2026-11-09-plugin-api-versioning.md)。标为 `main` 的源码链接是可变化的当前分支；历史数字版本例子固定到 `1.100.0` tag。

### 三种常见版本属于不同对象

| 位置 | 管理什么 | 不应误读为 |
| --- | --- | --- |
| 扩展 `package.json.version` | 扩展自己的发布版本，采用 SemVer | 宿主接口版本 |
| `package.json.engines.vscode` | 这个扩展兼容的 VS Code 产品版本范围，用来限制可安装的客户端 | 让宿主载入一整套旧版本接口 |
| `devDependencies.@types/vscode` | 开发时使用的 `vscode` 类型声明版本 | 扩展携带的运行时实现 |

前两项由[清单字段](https://code.visualstudio.com/api/references/extension-manifest)与[发布兼容性说明](https://code.visualstudio.com/api/working-with-extensions/publishing-extension#visual-studio-code-compatibility)定义。类型声明与宿主最低版本的关系也有工具校验：`vsce` 检查 `@types/vscode` 的 major/minor 不高于 `engines.vscode` 对应基线；patch 不按同一方式比较，因为 DefinitelyTyped 会独立增加类型包 patch。[vsce validation.ts](https://github.com/microsoft/vscode-vsce/blob/main/src/validation.ts)

以下仅为数字含义示例，不是推荐使用某个当前版本：

```json
{
  "version": "2.3.0",
  "engines": { "vscode": "^1.100.0" },
  "devDependencies": { "@types/vscode": "1.100.0" }
}
```

它表示扩展自身发布到 2.3.0，使用 VS Code 1.100 的类型基线，声明兼容 1.100.0 及其后的同一 major 产品版本。需要 1.105 新增稳定接口时，应相应提高宿主最低版本，而不能只升级类型声明后继续声称支持旧宿主。公开文档通过 `^1.8.0` 与精确 `1.8.0` 对比解释了这种范围语义；宿主校验器也保留 major/minor/patch 的范围检查。[发布兼容性说明](https://code.visualstudio.com/api/working-with-extensions/publishing-extension#visual-studio-code-compatibility)、[extensionValidator.ts](https://github.com/microsoft/vscode/blob/main/src/vs/platform/extensions/common/extensionValidator.ts)

扩展运行时读取的 `vscode.version` 明确是编辑器版本；当前宿主实现从 `initData.version` 提供该值。这里没有“安装 `@types/vscode@1.100` 就在宿主里切换到 1.100 运行时”的语义。[vscode.d.ts](https://github.com/microsoft/vscode/blob/main/src/vscode-dts/vscode.d.ts)、[extHost.api.impl.ts](https://github.com/microsoft/vscode/blob/main/src/vs/workbench/api/common/extHost.api.impl.ts)

### 稳定接口存在长期兼容承诺

官方 2016-04-14 的 1.0 发布说明明确宣布接口稳定，并承诺之后的兼容性。当前公开文档进一步表述为尽最大努力避免破坏性接口变更，扩展作者可以预期已发布扩展继续工作。因此准确结论是存在明确的长期兼容目标和承诺；不能因为未发现某种版本窗口政策，就说 VS Code 没有兼容保证。[VS Code 1.0 发布说明](https://code.visualstudio.com/blogs/2016/04/14/vscode-1.0)、[Using Proposed API](https://code.visualstudio.com/api/advanced-topics/using-proposed-api)

维护流程要求新提案若修改已有接口，必须向后兼容；接口指南要求保守公开能长期维持的最小表面。稳定化之前先通过提案迭代，完成后移入 `vscode.d.ts`。[Extension API process](https://github.com/microsoft/vscode/wiki/Extension-API-process)、[Extension API guidelines](https://github.com/microsoft/vscode/wiki/Extension-API-guidelines)

本轮核对的公开机制以宿主版本范围、稳定接口增量演进和独立实验提案为主，没有找到“稳定接口仅支持 N/N−1/N−2，随后自动淘汰”的公共政策，因此不能把 Nanobug 的前两个旧版本策略称作 VS Code 的原样做法。也不能由这一检索结果推导所有历史接口永远不变、所有扩展在任何新版本都绝无兼容问题；这里应保留当前官方的 best-effort 表述。

`deprecated` 也不等于立即删除。例如 `workspace.rootPath` 仍在当前 `vscode.d.ts` 中，标记改用 `workspaceFolders`；运行时 getter 仍返回结果，同时报告弃用信息。弃用模块会向开发中的扩展输出迁移警告，并记录去重后的使用信息，辅助迁移。这是可核实的保留实例，不是所有弃用接口永久保留的证明。[vscode.d.ts](https://github.com/microsoft/vscode/blob/main/src/vscode-dts/vscode.d.ts)、[extHost.api.impl.ts](https://github.com/microsoft/vscode/blob/main/src/vs/workbench/api/common/extHost.api.impl.ts)、[extHostApiDeprecationService.ts](https://github.com/microsoft/vscode/blob/main/src/vs/workbench/api/common/extHostApiDeprecationService.ts)

### Proposed API 使用独立实验流程

面向一般扩展作者的公开流程是：在 Insiders 上测试，通过 `enabledApiProposals` 声明提案名称，取得对应 `vscode.proposed.<name>.d.ts`。这些接口可变化，不应随普通 Marketplace 扩展发布；可以打包 VSIX 供测试者安装，并按文档显式启用。最新提案类型与较旧 `@types/vscode` 还可能不匹配，需要选取对应来源。[Using Proposed API](https://code.visualstudio.com/api/advanced-topics/using-proposed-api)

实验接口稳定化后，作者删除提案声明和单独的 proposal 类型文件，改用包含正式接口的 `vscode.d.ts`，并检查最终签名变化。这个过程把实验阶段的快速修改与正式接口承诺分开。[Extension API process](https://github.com/microsoft/vscode/wiki/Extension-API-process)

**内部机制与公开约定要分开。** 当前宿主源码存在产品配置 `extensionEnabledApiProposals`、开发模式与命令行开关等放行路径；未获准的非内置扩展会被清空提案列表。`vsce` 默认拒绝未放行的 proposed API 发布，但也存在显式绕过参数。这些路径解释了为何不能声称实验接口在所有构建和所有扩展中绝对不可用；它们不等于一般作者获得稳定接口承诺，也不能据此推断 Marketplace 服务端一定接受任意发布。[extensionsProposedApi.ts](https://github.com/microsoft/vscode/blob/main/src/vs/workbench/services/extensions/common/extensionsProposedApi.ts)、[vsce publish.ts](https://github.com/microsoft/vscode-vsce/blob/main/src/publish.ts)

**提案数字版本有历史证据，但不是全局稳定接口版本。** 在 `1.100.0` tag，提案注册表包含可选数字 `version`，解析器支持 `proposalName@version`，校验器在启用校验时对明确填写的数字做一致性检查。这是某项未稳定提案的修订检查，不是公共稳定接口的 N−2 支持制度。[1.100.0 提案注册表](https://github.com/microsoft/vscode/blob/1.100.0/src/vs/platform/extensions/common/extensionsApiProposals.ts)、[1.100.0 提案解析](https://github.com/microsoft/vscode/blob/1.100.0/src/vs/platform/extensions/common/extensions.ts)、[1.100.0 校验器](https://github.com/microsoft/vscode/blob/1.100.0/src/vs/platform/extensions/common/extensionValidator.ts)

本轮读取的 `main` 注册表与校验器没有展示上述相同的数字版本字段和检查，因此本文将它限定为已核实的历史实现，不把旧源码检查描述为当前所有发布渠道的通用规则。[main 提案注册表](https://github.com/microsoft/vscode/blob/main/src/vs/platform/extensions/common/extensionsApiProposals.ts)、[main 校验器](https://github.com/microsoft/vscode/blob/main/src/vs/platform/extensions/common/extensionValidator.ts)

### 对“早期固定为 1.0”的限定

可以确认的是 VS Code 在产品 1.0 发布时宣布接口稳定；本轮没有查到“接口尚未稳定，但刻意把接口显示版本固定为 1.0 来管理早期生态”的一手历史依据。`engines.vscode`、`@types/vscode` 和 `vscode.version` 都不能充当这个说法的证据。现有宿主版本校验器反而明确处理 0.x 与 1.x 兼容范围的差别。[1.0 发布说明](https://code.visualstudio.com/blogs/2016/04/14/vscode-1.0)、[版本校验源码](https://github.com/microsoft/vscode/blob/main/src/vs/platform/extensions/common/extensionValidator.ts)

**研究判断及后续决定。** 用户现已采用“实验能力与稳定承诺分离、明确最低宿主要求、发布前检查兼容性、弃用时提供迁移路线”。当前 API 显示 `1.0` 的人工改版要求仍保留，实际兼容性另查宿主和契约；Q18 已确认基础接口优先稳定、UI SDK 与自定义绘图先实验交付，验证成熟后逐项转稳定。

## 2026-11-09 补充：LazyVim 的 Lua 条件如何处理未知配置格式

本节核对当日官方文档及 lazy.nvim `main` 源码，源码链接会随上游变化；未运行 Neovim、性能测试或修改 Nanobug 实现。LazyVim 官方说明插件配置沿用 lazy.nvim 的 spec 机制。本问是机制比较，不表示 Nanobug 改用 Lua 或改变进程隔离设计。[LazyVim 插件配置](https://www.lazyvim.org/configuration/plugins)

**格式由普通 Lua 代码处理。** `enabled`、`cond` 可以是布尔值，也可以是返回布尔值的 Lua 函数。Neovim 内置 Lua，引擎提供编辑器 API、标准库和 Lua 模块加载；配置作者可以读取文件，调用已具备的解析模块，或自己实现所需解析逻辑，再返回是否启用。lazy.nvim 不需要先内置该项目配置格式，但也不会自动理解一个没有解析实现的新格式。[Plugin Spec](https://lazy.folke.io/spec)、[Neovim Lua 文档源码](https://github.com/neovim/neovim/blob/master/runtime/doc/lua.txt)

- **返回语义不同于异步任务。** 文档签名为 `enabled: fun(): boolean`、`cond: fun(LazyPlugin): boolean`。`enabled=false` 把插件排除出有效 spec，已有安装可能进入后续清理列表；`cond=false` 也禁止加载，但保留其安装，不把它作为未使用插件清理。当前实现直接调用 `enabled()` / `cond(plugin)`，以 Lua 真值判断返回值；作者应按文档返回布尔值。这不是返回 Promise 后由管理器等待的接口，也不是设置 `enabled=false` 就立即删除插件目录。[Plugin Spec](https://lazy.folke.io/spec)、[meta.lua](https://github.com/folke/lazy.nvim/blob/main/lua/lazy/core/meta.lua)、[plugin.lua](https://github.com/folke/lazy.nvim/blob/main/lua/lazy/core/plugin.lua)
- **执行发生在 spec 解析/重新解析时。** `Loader.setup()` 调用 `Plugin.load()`，后者解析 spec；`Meta:resolve()` 内执行 `fix_cond()`、`fix_disabled()`。普通插件加载路径只检查保存的 `plugin._.cond`，不会在每个文件或懒加载事件到来时自动重新执行条件函数。因此不能把 `cond` 理解成每次插件调用前重新检查的动态条件；也不能因插件设置懒加载就假定条件读取会推迟到首次使用。[meta.lua](https://github.com/folke/lazy.nvim/blob/main/lua/lazy/core/meta.lua)、[plugin.lua](https://github.com/folke/lazy.nvim/blob/main/lua/lazy/core/plugin.lua)、[loader.lua](https://github.com/folke/lazy.nvim/blob/main/lua/lazy/core/loader.lua)
- **配置热重载不等于读取依赖跟踪。** 内置变更检测遍历 `Config.spec.modules` 对应的 Lua 配置模块，比较文件状态并触发 `Plugin.load()`；它没有自动登记条件函数读过的任意项目配置文件。条件读取某个自定义数据文件后，该文件变化不会仅因此自动触发重新判断；需要作者另行设计事件/监听与刷新。[reloader.lua](https://github.com/folke/lazy.nvim/blob/main/lua/lazy/manage/reloader.lua)
- **调用路径没有自动资源隔离保证。** 上述条件是在 Neovim 的 Lua 执行路径直接调用，并未为每个条件创建隔离进程、自动异步执行器或执行超时中断。由源码可推断，同步的大文件读取或长时间解析会延长 spec 解析，发生于启动解析时也会拖慢启动。作者可以自行安排异步预处理、缓存或外部进程，但不能把这些当作 `enabled` / `cond` 自带保障。[meta.lua](https://github.com/folke/lazy.nvim/blob/main/lua/lazy/core/meta.lua)、[loader.lua](https://github.com/folke/lazy.nvim/blob/main/lua/lazy/core/loader.lua)、[Neovim Lua 引擎](https://github.com/neovim/neovim/blob/master/runtime/doc/lua.txt)

**后续决定：用户已回答“借鉴吧”。** Nanobug 采用宿主通用文本 API 配合规则作者轻量 JS 解析代码的扩展方向，继续保留已确认的独立规则进程、异步读取、依赖跟踪、缓存与时间预算；不照搬 lazy.nvim 的同步执行和条件缓存方式，也不切换为 Lua。具体接口、辅助代码打包、重型解析服务及 Q32 宿主筛选清单仍待确定，未实现或实测。
