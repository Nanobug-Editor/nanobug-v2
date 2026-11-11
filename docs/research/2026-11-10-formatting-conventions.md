# 格式化常规交互：参考 VS Code 与 IntelliJ IDEA

日期：2026-11-10。性质：官方资料核对与 Nanobug 设计整理，未实现或运行格式化功能。

用户要求停止逐项追问格式化细节，按照 IDEA 和 VS Code 的相似做法收敛。后续常规交互由实现方依据本记录和已确认约定处理，不再展开新的格式化问答树。下文区分官方事实与 Nanobug 自行选择；两款 IDE 并非每个行为都一致。

## 核实到的常规行为

| 主题 | 官方证据 | Nanobug 的取用方式 |
| --- | --- | --- |
| 手动格式化范围 | VS Code 提供整文档和选区两个明确命令；IDEA 的 Reformat Code 在有选区时处理选区，无选区时处理整文件。[VS Code](https://code.visualstudio.com/docs/editing/codebasics#_formatting)、[IDEA](https://www.jetbrains.com/help/idea/reformat-and-rearrange-code.html#reformat_code) | 提供整文档与选区命令；选区格式化仅在提供者支持相应能力时可用，不能把选区请求悄悄扩大为整文件。 |
| 保存时格式化 | VS Code 基础编辑文档说明输入、保存和粘贴触发的格式化默认关闭，可通过设置启用。IDEA 在 Actions on Save 中单独启用 Reformat code，并可设置文件类型、排除范围及整文件或已改行。[VS Code](https://code.visualstudio.com/docs/editing/codebasics#_formatting)、[IDEA](https://www.jetbrains.com/help/idea/reformat-and-rearrange-code.html#reformat_on_save) | 保存时格式化作为独立可选设置，初版默认关闭。用户已经确认的等待上限不代表功能默认开启。 |
| 默认提供者 | VS Code 官方 Python 文档展示了选择默认格式化器、按语言设置 `editor.defaultFormatter` 的入口；同一文档也说明并非每个工具都支持选区格式化。[VS Code Python](https://code.visualstudio.com/docs/python/formatting#_set-a-default-formatter) | 保留 Q57：按功能与文件类型指定默认插件、项目覆盖全局；能力不足时明确说明，故障不临时换用其他插件。 |
| 项目风格 | VS Code 支持用户、工作区及语言范围的设置，其完整优先级比简单两层覆盖更丰富。IDEA 区分项目代码风格与 IDE 级方案。[VS Code 设置](https://code.visualstudio.com/docs/configure/settings#_settings-precedence)、[IDEA 代码风格](https://www.jetbrains.com/help/idea/configuring-code-style.html) | 借鉴项目风格可共享的原则，继续使用已确认的 `.nb/` 与 Apply 规则，不照搬两家的目录及所有设置层级。 |
| EditorConfig | IDEA 通过捆绑且默认启用的 EditorConfig 插件读取配置；更近的文件优先，未指定项可回退项目代码风格。IDEA 专属 `ij_` 属性不等于通用标准。[IDEA EditorConfig](https://www.jetbrains.com/help/idea/editorconfig.html) | 允许格式化提供者采用其支持的项目风格文件。是否识别某种工具配置由该提供者声明；不声称所有插件都支持所有 EditorConfig 属性。 |
| 编辑与撤销 | IntelliJ Platform 要求文档修改放在 command 中，最外层 command 进入撤销栈。[IntelliJ Documents SDK](https://plugins.jetbrains.com/docs/intellij/documents.html#what-are-the-rules-of-working-with-documents) | 这支持把一次格式化看作一项可撤销编辑的方向；Nanobug 单文档整批校验、一次应用与一次撤销仍以 Q59 为具体契约。 |

## 必须保留的差异与证据边界

- VS Code 默认需要显式保存，并显示未保存标记；IDEA 默认会在多种事件发生时自动保存，也提供显式保存。因此不能把“手动格式化完成后一定等用户再按保存”说成两家共同保证。[VS Code 保存](https://code.visualstudio.com/docs/editing/codebasics#_save-auto-save)、[IDEA 保存](https://www.jetbrains.com/help/idea/saving-and-reverting-changes.html)
- IDEA 文档将已启用的保存动作关联到保存事件；VS Code 当前源码的 `FormatOnSaveParticipant` 会在关闭相应设置时返回，也跳过 `SaveReason.AUTO`。不能仅凭名称就宣称所有类型的自动保存都会触发格式化。[IDEA 保存动作](https://www.jetbrains.com/help/idea/saving-and-reverting-changes.html#actions-on-save)、[VS Code saveParticipants.ts](https://github.com/microsoft/vscode/blob/main/src/vs/workbench/contrib/codeEditor/browser/saveParticipants.ts)
- VS Code 的 `formatDocumentWithProvider` 等待提供者结果、检查取消，再通过编辑操作应用结果；该函数没有直接发起保存。它为“格式化修改文档缓冲区，保存有独立流程”提供局部源码证据，不能据此推断完整并发行为。[VS Code format.ts](https://github.com/microsoft/vscode/blob/main/src/vs/editor/contrib/format/browser/format.ts)
- 本次公开文档没有确定两家在“正在手动格式化时再次保存”“范围不同的任务同时到达”“重复失败提醒如何合并”上的统一规则。下述处理是 Nanobug 的设计选择，不能写成 VS Code 或 IDEA 已验证的相同行为。源码链接为本次读取的 `main`，未据此宣称固定发行版的所有边界行为。

## Nanobug 已有决定继续有效

以[插件架构决策 Q57–Q63](2026-11-08-plugin-architecture.md)及用户原话为准：

- 默认插件可按项目覆盖全局，修改按 Apply 生效；项目只指定插件身份，不锁定插件包版本。
- 格式化失败仅终止并消息提醒，由用户处理；不自动重试，也不临时更换插件。
- 单文档结果整批校验和应用，一次撤销；文档变化或请求失效后，拒绝旧结果。
- 保存格式化初始等待上限一秒；手动格式化初始上限十秒，允许取消。
- 相同手动请求共用任务且不重置计时；开启保存时格式化且可复用时，采用原手动截止时间与保存起一秒中更早的截止时间。

这些数值和严格失效策略是 Nanobug 的明确决定，不是从两款 IDE 文档导出的行业统一标准。

## 授权下选用的常规交互

以下为在用户授权下采用的设计推导，已同步到[架构记录第 71 节](2026-11-08-plugin-architecture.md)，不再列为逐项待用户确认的问题；实现与验收仍需覆盖相应状态变化。

1. **保存与手动格式化分开。** 未启用保存时格式化时，保存不额外启动或等待格式化。仍有效的已有手动任务可以继续；后到结果若确实改变正文，则显示未保存状态，不把之前的写盘成功算到新版本上。该任务本身不额外发起自动写盘；另有自动保存策略时按其独立配置处理。
2. **不同范围或配置的任务不误合并。** 已开启保存时格式化、但保存请求与当前手动任务不相同时，采用保存流程优先：使旧手动请求失效，再按保存的一秒总预算处理需要的格式化，拒绝旧任务迟到结果，不允许两份结果竞争修改正文。旧任务的取消不等于后台资源已经立即释放，也不能重置无关任务的预算。此项是 Nanobug 为兑现保存等待上限作出的调度选择，不是已核实的双 IDE 共同实现。
3. **格式化不是隐含修复合集。** 格式化、整理导入、代码修复分别作为能力和设置；未启用的额外操作不随普通格式化顺带执行。IDEA 本身也将这些操作分列，但 Nanobug 无须首版实现其全部保存动作。[IDEA 格式化选项](https://www.jetbrains.com/help/idea/reformat-and-rearrange-code.html#reformat_file)
4. **提示简短、完整原因可查。** 手动失败显示一次非阻塞消息并给出日志入口；连续自动保存遇到同一插件、同一原因时合并提醒、更新次数，日志保留每次结果。恢复后清除过期故障状态。该去重策略由 Nanobug 自行选择，不能归因于两家统一规范。
5. **命令和帮助使用已有键盘体系。** 整文档、选区、取消及设置入口沿用 Nanobug 的命令和快捷键帮助，不照搬 IDEA/VS Code 的物理快捷键。实际实现字符按钮时，按仓库 AGENTS.md 的 `&` 助记字母与各语言文案约定绘制和匹配；本次没有修改 UI。

Q64 此前只是未获单独采纳的提案。用户最新指示改变了协作方式：剩余常规细节改由参考调研与工程判断收敛，不应把其历史状态篡写成“用户逐条确认了 Q64”。后续问答返回插件性能、接口边界、扩展能力和故障隔离等实质架构选择。
