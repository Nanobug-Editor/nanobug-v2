# 插件运行时机制研究

调查开始：2026-11-08，决策持续更新。范围：Nanobug V2 的 WASM 加载、调度、资源约束及 JS 启用规则。本文记录一手资料、已确认方向与候选设计；尚未进行 Nanobug 插件性能实测。Q24 已确定规则时间的初始预算，具体选型及其余资源限额仍待设计。

## 项目前提与证据范围

需求文件明确要求首屏及编辑不等待重型插件初始化、插件自主实现界面、每个已激活插件一个 runtime 进程，以及 JS 仅判断模块启用条件。UI/业务/grammar 分别按需加载仍是待实现方向。[需求总结：R02、R06–R12及插件章节](../需求总结.md)

本次读取的 `Cargo.toml` 没有 Wasmtime、Tree-sitter 或 QuickJS 依赖，因此下述机制不是已经接入的项目能力。编辑核心现状应以同期源码调查为准，不能仅依据 manifest 的 Neovim 描述推断。[Cargo.toml](../../Cargo.toml)

在线文档属于滚动资料：本次 Wasmtime Rust 文档页头为 `52.0.0-dev`，Tree-sitter Rust 文档为 `0.27.2`，rquickjs 为 `0.14.0`。这些版本号只说明所读页面，不代表建议选择开发版或已完成版本兼容验证。[Wasmtime](https://docs.wasmtime.dev/api/wasmtime/)、[Tree-sitter](https://docs.rs/tree-sitter/latest/tree_sitter/struct.WasmStore.html)、[rquickjs](https://docs.rs/rquickjs/latest/rquickjs/struct.Runtime.html)

## 已核实的机制

| 主题 | 一手资料结论 | 对 Nanobug 的含义（设计推断） |
| --- | --- | --- |
| 编译与实例化 | Wasmtime `Module::new` 执行完整同步编译；`Module` 是可在线程间共享、廉价克隆的已编译代码。序列化后可在后续加载时省去编译。[Module](https://docs.wasmtime.dev/api/wasmtime/struct.Module.html) | 首次加载 WASM 不能放进 UI 线程。需要区分读取、验证/编译、实例化、模块初始化四段耗时。 |
| 本机预编译 | `Engine::precompile_module`/`precompile_component` 的产物面向兼容目标与配置；`precompile_compatibility_hash` 可判断引擎配置兼容性。[Engine](https://docs.wasmtime.dev/api/wasmtime/struct.Engine.html#method.precompile_compatibility_hash) | 跨平台分发包保留 WASM；缓存由本机生成，按模块内容、运行时版本、目标及配置区分。缓存不等于跨平台安装包。 |
| 缓存信任 | `Module::deserialize` 为 `unsafe`，只轻量检查输入；任意字节可形成任意原生代码执行。文件映射版本还要求映射期间文件保持有效且不变。[反序列化约束](https://docs.wasmtime.dev/api/wasmtime/struct.Module.html#method.deserialize) | 仅加载来源受控的编译产物。来自插件包的任意“预编译缓存”不能按普通 WASM 信任；内容哈希用于标识，不单独证明来源可信。 |
| 编译移出关键路径 | 官方预编译文档把编译移出启动关键路径作为主要收益；预编译文件可按需映射。[Pre-Compiling Wasm](https://docs.wasmtime.dev/examples-pre-compiling-wasm.html) | 安装、升级或后台空闲时编译候选模块；没有缓存时允许功能稍后就绪，编辑仍继续。收益必须在本机测量。 |
| 实例化成本 | 官方建议包括预解析导入的 `InstancePre`、写时复制初始化、池化分配器；池化会提前预留并发实例所需资源。[Fast Instantiation](https://docs.wasmtime.dev/examples-fast-instantiation.html) | 可先采用稳定导入对应的 `InstancePre`。每插件通常只有少量长期实例，是否启用大池必须比较内存与重启收益，不能照搬服务器配置。 |
| Engine 与 Store | 官方多线程示例共享 Engine、已编译 Module 和 Linker，各执行线程创建自己的 Store；Engine 是进程内可共享的编译与运行环境。[多线程嵌入](https://docs.wasmtime.dev/examples-multithreaded-embedding.html) | 每个插件 runtime 内复用 Engine；UI 与重型业务采用不同 Store 和调度通道。共享 Engine 不表示多个进程共享同一个 Rust 对象。 |
| 模块卸载 | Store 内创建的实例随 Store 销毁才释放，普通实例不会在长期 Store 中单独自动回收；文档建议 Store 生命周期接近主要实例生命周期。[Store](https://docs.wasmtime.dev/api/wasmtime/struct.Store.html) | 若模块需要独立卸载和重建，Store 应对应这个生命周期。把所有历次加载实例塞进一个永久 Store 会妨碍回收。 |
| async 的范围 | Wasmtime 的异步执行体现为 Future；真正执行发生在 poll，线程池由嵌入方管理。[Async](https://docs.wasmtime.dev/api/wasmtime/#async) | `async` 本身不会把 CPU 工作移开 UI 线程，也不会自动提供插件优先级。执行器、工作线程与队列仍由 runtime 负责。 |
| 中断结果 | fuel/epoch 可选择 trap 或异步 yield；trap 终止本次执行且不能从该位置续跑，yield 则把调度机会交回宿主。[Interrupting Execution](https://docs.wasmtime.dev/examples-interrupting-wasm.html) | 取消、超时失败与公平调度需要不同处理。切片不能直接当作失败，失败也不能承诺无损续跑。 |
| fuel 与 epoch | fuel 适合确定性的执行量计数；epoch 依赖外部推进计数和 Store 截止点，适合较低开销的粗粒度中断。二者都不能解除已经阻塞的 host call；批量内存操作也可能延长切片。[Config](https://docs.wasmtime.dev/api/wasmtime/struct.Config.html#method.epoch_interruption) | 前台与后台采用不同预算；I/O、原生库和锁等待另设异步/超时策略。不能把“每隔若干毫秒推进 epoch”当作严格端到端时限。 |
| 内存限制范围 | ResourceLimiter 约束实例内存、表等资源，不覆盖 Wasmtime 全部内部内存及宿主分配；CPU 也需另管。[ResourceLimiter](https://docs.wasmtime.dev/api/wasmtime/trait.ResourceLimiter.html) | 还需限制 IPC 字节、宿主资源句柄、图片、缓存和排队任务。WASM 内存上限不能被当作整个插件进程的 RSS 上限。 |
| 栈与宿主调用 | Wasm 栈上限不保证调用线程实际有足够栈空间，也不限制任意宿主函数的栈使用；配置不当仍可导致进程终止。[栈配置](https://docs.wasmtime.dev/api/wasmtime/struct.Config.html#method.max_wasm_stack) | 每插件进程仍有价值；WASM 限制不能替代进程监控与恢复。 |

## 候选加载与执行结构

下面是根据上述事实及现有需求提出的结构，尚待用户确认。它保留每插件一个进程，不要求每个 WASM 模块再创建进程。

```text
编辑器宿主
  ├─ 核心编辑、输入、光标、实际绘制
  ├─ 插件目录索引及配置条件结果汇总
  ├─ 共享规则判断进程 → 执行各插件 JS 配置（已确认）
  └─ runtime 生命周期管理、资源预算、异步消息
       └─ 一个已激活插件的 runtime 进程
            ├─ 一个可复用 Engine / 已编译代码缓存
            ├─ UI 执行通道 → UI Store → 批量绘制数据
            ├─ 有并发上限的业务通道 → 独立 Store
            └─ 有预算的宿主函数适配器 / 可取消 I/O
```

推荐把职责集中到少量深模块中：配置管理给出选中的模块，调用方提交某版本任务或明确关闭，由生命周期管理隐藏去重启动、依赖加载、超时与重建。用户在第二轮明确配置条件满足即放行，否决闲置回收；不能再叠加“首次实际使用才激活”的判断。后续 Q8 已确认项目仍打开时不因闲置回收，项目关闭时释放独占资源，插件最后一个资源所有者结束后收尾并退出进程；具体任务取消与收尾协议仍待设计。

候选规则：

1. Q30 已确认核心界面及当前场景基本操作就绪后，自动启动公共规则进程并安排条件满足的普通插件加载，不等待实际输入；只浏览项目或只读文档也适用。Q34 已确认安装、更新时提前准备清单索引及运行时支持的本机编译缓存，换取以后启动时少做准备工作；缓存缺失或失效时在核心界面就绪后后台重建。该决定不执行条件未选中的业务模块，不承诺未选 JS 引擎的缓存能力。具体就绪信号、缓存策略及首次插件可用时限待设计与测量。
2. WASM 编译与进程启动都设并发上限。即使移出 UI 线程，几十个插件同时抢占 CPU/磁盘仍会影响输入延迟。
3. UI 与重型业务不共享必须长时间持有的 Store 或全局锁；前台任务有独立调度份额，后台任务可取消或被暂停。线程数与优先级必须纳入多插件场景实测。
4. 未变化的插件界面复用宿主已经接收的绘制数据；插件只在状态变化时提交批次。宿主不在每帧同步询问插件，也不以逐字符调用作为跨进程粒度。Q35 已确认变化积压时跳过可替代的过时画面：仅在最新完整状态可有效呈现时替代旧状态，增量须正确合并或补齐依赖；输入、编辑、保存及有副作用操作不随画面丢弃。具体队列和帧率未确定。
5. 按用户第二轮修正，启用规则直接决定模块是否放行，不仅是候选资格；条件满足后不再等待额外使用需求，也不因闲置回收。跨文档条件结果仍需汇总，避免一个不匹配上下文关闭其他上下文仍选中的模块。
6. 独立 Store 之间用消息和明确数据句柄交互；共享内存、线程扩展及更复杂的并发调用只在测量证明需要后考虑。

独立 Store 和执行通道用于分别调度、限制与重建模块；它们仍处在同一个插件进程中。该进程若因原生代码、栈或其他故障终止，UI 与业务 Store 会一起失效。因此“业务任务不阻塞 UI”与“业务所在进程崩溃时 UI 仍存活”是两种不同保证；在每插件一个进程的要求下，后者只能由宿主保留最后有效绘制数据并重建插件来缓解。[栈与宿主函数约束](https://docs.wasmtime.dev/api/wasmtime/struct.Config.html#method.max_wasm_stack)

这些规则是设计推断，不是 Wasmtime 自动提供的能力。它们对应需求中的首屏独立、模块化、每插件进程和批量绘制方向。[需求总结](../需求总结.md)

## Tree-sitter 需要单独的加载适配器

Tree-sitter 的 Rust 接口提供 `WasmStore::new`、`load_language`，并由 `Parser::set_wasm_store` 接入解析器；`set_language` 会拒绝不可用或 ABI 不兼容的语言。这是该解析器自己的接口，不能仅凭 `.wasm` 扩展名把 grammar 当作业务 WIT 导出加载。[WasmStore](https://docs.rs/tree-sitter/latest/tree_sitter/struct.WasmStore.html)、[Parser](https://docs.rs/tree-sitter/latest/tree_sitter/struct.Parser.html)

建议保留 grammar 加载适配器以及解析任务的文档版本检查。解析放在插件 runtime 还是集中解析模块中，仍是待决事项；本文不默认采用共享进程。对嵌入语言，只加载必要 grammar/查询，不由代码块高亮请求自动拉起整套 LSP，符合既有需求方向。[需求总结：语言选择和嵌入高亮](../需求总结.md)

还需实测确认 Tree-sitter 使用的 Wasmtime 版本、它暴露的取消/资源限制路径和插件业务运行时能否复用 Engine。公开接口表明二者存在独立接入路径，不足以证明直接共享所有调度或限制设置。

## JS 规则必须有独立预算

QuickJS 提供 runtime 内存上限、栈上限与定期执行的中断回调；同一个 JSRuntime 内不支持并行多线程访问，不同 JSRuntime 不直接交换对象。[QuickJS 的 runtime 与执行控制](https://bellard.org/quickjs/quickjs.html#Runtime-and-contexts)

rquickjs 提供对应的 `set_interrupt_handler`、`set_memory_limit` 和 `set_max_stack_size`；所读版本明确指出：使用自定义 allocator，包括相关 `rust-alloc`/`allocator` 功能时，`set_memory_limit` 不起作用，必须验证实际分配器是否执行预算。[rquickjs Runtime](https://docs.rs/rquickjs/latest/rquickjs/struct.Runtime.html)

**Q20 后续修正：规则可以主动调用宿主数据 API。** 宿主提供常用文件读取及其他用于判断的 API，JS 依据 API 返回的数据计算各模块的 `true`/`false`，宿主执行启用/调用结果；不再将数据限定为事先声明并预填的上下文。Q21 已确认可能等待的 API 使用异步调用，具体 API 范围仍未冻结。规则执行在 UI 线程之外，单次时间、内存及输入规模需要预算；脚本出错保留有效配置，但不能永久沿用旧上下文结果。以上是宿主需要落实的设计，不能单靠引擎名称获得；引擎执行控制机制仍参考前述官方资料。

用户在 Q5 解释后已确定：所有插件共用一个独立规则判断进程执行 JS 配置。这样无需先启动各插件 runtime 才能判断模块条件，并可单独终止/恢复规则执行环境。Q19 已确认下面的故障行为，Q24/Q25 已确定初始时间预算与进程恢复次数；进程启动阶段、上下文同步及预算执行机制仍需设计。此确认不包括共享 Tree-sitter 解析进程。

**Q19 已确认：规则失败不扩大为所有插件失效。** 中止报错或超时的判断并显示原因；规则进程崩溃时单独恢复，不连带终止插件 runtime。已有成功结果仅在有效配置及相关上下文仍匹配时复用；新项目、新配置或变化后的上下文没有有效结果时，暂不放行依赖该判断的新调用。已有实例与草稿按 Q8 保留，正文继续可编辑，规则修复后重新评估。保留上一有效配置不等于任意沿用旧上下文的 true；结果需要关联所属项目/文档及配置、上下文版本。具体版本结构与强制中止手段尚待实现，进程恢复次数按 Q25 单独确认的参数执行，与插件 runtime 独立计数。

Q20 已允许脚本通过宿主 API 请求文件数据，不再要求先声明所有所需字段。API 服务由宿主管理，不为取得启用判断数据而先启动被判断插件的重型模块。“其他 API”的完整范围、路径权限与限额仍需设计，不能由该表述推导出任意网络或命令能力已经确定。

**Q21 已确认：涉及 I/O 的 API 使用异步等待。** 用户已采纳文件读取等可能等待的 API 使用异步调用。宿主在 UI 线程之外处理读取，通过 Promise/`async/await` 返回数据；规则等待时让出执行机会，其他可运行规则继续推进。已在内存中的上下文可直接读取。缓存命中不改变同一 API 的异步签名，过期或被取消的判断结果不用于放行。初始计算与整体完成预算见 Q24；具体调度、取消和单个宿主调用的资源收尾仍待设计，尚未实现或验证。

**Q22 已确认：宿主自动跟踪读取依赖。** 用户已采纳由宿主记录规则通过数据 API 实际读取/查询的对象，相关数据变化后更新缓存并合并调度受影响规则。实现还需覆盖“不存在”的查询结果和动态分支，求值失败后仍能发现数据修复；项目关闭时移除独占依赖与订阅。过期结果按 Q19/Q21 拒绝。具体监听、遗漏后的重新核查、缓存容量及刷新时限待设计，当前尚未实现或验证。

**Q23 已确认：磁盘与文档读取分开。** 文件读取 API 默认读取磁盘已保存内容，单独文档 API 读取包含未保存修改的编辑器文档；规则作者显式选择。宿主分别记录来源及版本，并按对应数据变化重算，不因路径相同而混用缓存。文档不存在时不静默改读磁盘；具体 API 和快照格式待设计，尚未实现或验收。

**Q24 已确认：分开限制计算与整体完成时间。** 用户已采纳每次判断累计 20 ms JS 计算、从开始执行起 5 s 内产生有效结果作为初始预算，后续根据实测调整；整体时限包含 API 等待，`await` 不重置累计预算。数值尚未经过本机测量，计时与中断精度仍需验证。超限按 Q19 处理，使本次结果失效并尝试取消未完成 API，不把超时转成条件真假；不能据此承诺底层阻塞 I/O 立即停止。宿主 I/O 并发、内存和返回体大小等限额另行设计。

**Q25 已确认：共享规则进程的恢复次数。** 用户已采纳与插件 runtime 相同节奏但独立计数：任意 5 分钟最多自动重启 3 次，分别等待 1、3、10 秒；第 3 次成功则继续运行，再次故障且无额度时暂停自动恢复并提供手动重试。已有仍有效结果继续使用，新判断等待服务恢复。普通脚本报错或可中止的规则超时不触发公共进程重启。尚未实现或验收；数据读取重试策略见 Q27，进程卡死检测与恢复协议仍待设计。

**Q26 已确认：连续计算超限暂停单条规则。** 用户已采纳对同一规则、作用域和有效配置，统计正常触发判断的连续 JS 计算超限；达到 3 次时暂停该规则在该作用域的自动执行，修改规则或手动重试后恢复，一次成功判断清除计数。普通文件变化不清空计数或解除暂停；读取失败和整体等待超时不混入计算超限。其他作用域和规则继续工作，实例与草稿保留，旧结果仍须符合 Q19 的有效性要求。尚未实现或验收，状态归属与实现细节见主设计记录。

**Q27 已确认：只读 API 临时失败的有限重试。** 用户已采纳明确可重试的临时读取错误首次失败后最多重试 2 次，分别等待 100 ms、300 ms；包括首次总共最多读取三次，任一次成功即返回。全部尝试共用原判断的 5 秒完成期限，等待和重试不重置 JS 计算预算。只重试宿主读取请求，不重跑整段 JS。权限、路径、解析等非临时错误直接报告，数据变化或手动操作再触发判断；不存在的成功查询结果不算读取失败。次数与间隔是待实测的初始策略，错误分类、共享读取和取消机制仍待设计。

**Q28 已确认：每条规则、每个作用域保留一项执行和一项最新等待。** 用户已采纳数据连续变化时覆盖尚未执行的旧请求，下一次按最新有效数据判断。过期的在途判断可尝试取消，其旧结果不得放行；未完成或未确认取消前不并行启动同一规则与作用域的下一项。仍未停止的底层 I/O 单独计入宿主资源预算。不同作用域不相互覆盖，暂停规则与关闭项目按原规则处理。整体并发、公平调度和排队时限另行确定，尚未实现或验收。

**跨规则限量并行已确认。** 用户已采纳不同规则可以并行但控制总数量，为正文编辑保留计算能力；具体数量以实测确定，未选定 JS 引擎和执行器数量。Q28 的一项执行加一项最新等待只限制同一规则、同一作用域的重复判断，不要求所有规则串行。实现候选与引擎约束见本文末尾追加调查，当前尚未实现或验收。

**Q29 已确认：忙满时当前操作相关任务优先，同时让其他任务持续推进。** 用户已采纳有空余执行能力就安排任务，忙满需要排队时优先处理当前操作窗口、文件和项目的判断，其他项目和插件也轮流获得执行机会；焦点改变调整后续顺序，不改变配置条件或实例保留规则，不解除故障暂停。具体公平调度算法、权重及最大等待时间仍待设计，此原则不确定整体并发数，也不覆盖全部插件业务任务。当前尚未实现或验收。

**Q30 已确认：公共规则进程与普通插件准备工作延后到核心界面就绪。** 用户在澄清只看、不修改也会自动加载后回答“可以”。核心界面及该场景下的基本操作准备好后，宿主自动启动公共规则进程、执行条件并安排满足条件的插件加载。有可编辑正文时，就绪包括可以输入和保存，但不要求用户真的输入或保存；只浏览项目、没有打开正文或文档只读时，同样自动执行适用规则，不等一个可写文件出现。这样减少首个可编辑状态前的资源竞争，但可能推迟插件首次可用；仍需实测。轻量清单注册、就绪时间点及具体启动实现待设计，当前尚未实现或验收。

**Q31 已确认：不按源文件总大小拒绝查询，由宿主只返回符合条件的部分。** 用户采纳宿主在后台读取、解析与筛选，只向 JS 返回所需匹配数据，结果多时分批返回；无索引时仍可能扫描整个文件，查询未完成与没有匹配分别表达。Q24 的累计 JS 计算与整次判断预算继续适用，文件总大小不设门槛不等于返回结果或内存无限制。具体查询操作、回调形式、页大小、缓存和索引方案待设计，本文末尾追加机制证据。10 MB 上限及任意查询语言均未获确认，当前尚未实现或验收。

**Q32 待确认：常用筛选条件与组合。** 建议首版提供字段存在/相等/不等、数字大小/范围、文字包含/开头/结尾等条件，并允许同时满足、满足之一及取反组合。宿主先筛选，JS 对返回的必要数据继续计算启用结果。具体数据格式、类型语义、字段路径和函数签名仍需设计；这不等于对全部原始记录执行任意 JS 回调，提案尚未采纳。

**后续已确认：借鉴 LazyVim 的作者轻量解析方式。** 用户已采纳宿主提供通用文本 API，规则作者可用少量 JS 解析代码支持未知格式；继续保留独立规则进程、异步 I/O、依赖跟踪、缓存及 20 ms JS / 5 秒总判断预算。辅助解析属于同次规则工作，不因函数或模块拆分而重新计时。原 Q32 的宿主筛选清单仍未确认，独立重型解析服务也不由本次采纳自动确定。

**Q33 已确认：规则辅助 JS 随插件打包。** 用户已采纳轻量解析代码随插件交付，在公共规则进程中供条件函数使用，不依赖其 WASM 业务模块先启动，也无需用户另装解析库。辅助代码和规则共用既有预算；具体导入语法、加载与初始化的计时、代码缓存、版本及状态隔离待设计。该决定不承诺 Node.js 或任意 npm 兼容，当前尚未实现或验收。

## 高可用需要补齐的宿主行为

以下是结合“WASM 约束只覆盖部分资源”的事实提出的候选协议规则，不是对运行时现有自动能力的描述。[ResourceLimiter](https://docs.wasmtime.dev/api/wasmtime/trait.ResourceLimiter.html)、[阻塞宿主调用限制](https://docs.wasmtime.dev/api/wasmtime/struct.Config.html#method.epoch_interruption)

| 故障 | 建议处理 |
| --- | --- |
| 超时或 Wasm trap | 标记本次任务失败；需要时重建模块 Store；保留核心编辑与最后有效界面。 |
| runtime 进程退出 | 由宿主释放该进程拥有的资源与在途请求，递增会话代次。Q13 已确认每插件任意 5 分钟内最多自动重启 3 次，三次尝试分别等待 1、3、10 秒；再次故障且额度耗尽时暂停自动恢复，显示原因并允许手动重试。成功恢复的实例不因额度耗尽而被终止。 |
| 业务忙而 UI 正常 | 暂停/取消后台队列，避免仅因一次慢任务就杀死整个插件。 |
| 消息积压 | 队列和单条消息设上限；可替代的绘制帧只保留最新版本。编辑请求、保存及有副作用操作必须明确确认和错误语义，不能随意丢弃。 |
| 旧响应迟到 | 请求携带插件会话代次、文档版本及任务 ID；恢复后拒绝旧代次的回包，避免旧格式化结果覆盖新编辑。 |
| 重启后副作用不明 | 读取与可重建计算可重试；写文件、运行命令等操作不盲目自动重放，需幂等标识或查询实际结果。 |
| 资源超限 | 区分 Wasm 内存、宿主句柄、IPC、缓存和整个进程；超限后降级或关闭相关能力，避免宿主替插件无限承担内存。 |

心跳只能证明某条执行路径仍可响应，不能证明 UI、业务和在途任务都健康；应同时观测任务 deadline、队列长度与执行通道进度。这是故障模型推断。

## 实测设计与待决问题

在性能原型中分别计时：进程启动、Engine 初始化、缓存命中/未命中、实例化、模块初始化、首个可用界面、首个业务结果。缓存命中与未命中不能混成一个“启动耗时”。还应同时测输入延迟、CPU、进程私有内存及宿主承担的插件资源；这些是拟议验收方法，没有填入虚构数值。

最小实验矩阵建议包括：零插件基线；安装很多但未激活；少量与多量同时激活；UI 插件同时做 CPU 索引；慢 I/O；无限循环；Wasm 内存增长；超大绘制批次；运行中关闭文档；runtime 被终止后重建。对缓存还应覆盖首次安装、升级失效、缓存文件被截断与不可信产物被拒绝。

需要后续决策的事项：

- 首屏哪些插件能力必须立即可用，哪些允许在核心可编辑后补齐。
- 面板首次唤醒、总内存和后台 CPU 的目标及取舍。
- 已确认的宿主输入恢复与自动重启预算如何落实到运行代次、状态重同步及故障验收；卡死判定、共享规则进程的恢复仍待细化。
- 共享 JS 规则进程的启动、预算和恢复；Tree-sitter 的执行位置仍待决定。
- 正式 WASM/WIT 接口版本、选定运行时版本和支持的目标平台。

本次只建立机制证据与候选结构；任何“保证秒开”“严格几毫秒抢占”“插件绝不影响宿主”的结论，都需要具体预算、协议限制和故障实测支撑。

## 追加调查：关闭项目后的内存生命周期

追加日期：2026-11-08。问题是同时打开 Rust、Vue 项目后，关闭 Vue 能否释放相关内存，以及保留插件实例时能否保证进程占用下降。本节区分运行时事实和生命周期建议，未运行项目或给出实测内存数字。

### 实例、已编译代码与进程是不同生命周期

| 对象 | 已核实的所有权与释放行为 |
| --- | --- |
| `Instance` 句柄 | Wasmtime 的实例由创建它的 Store 拥有；Instance 是可复制句柄。只丢弃句柄不会销毁 Store 中的实例。[Instance](https://docs.wasmtime.dev/api/wasmtime/struct.Instance.html) |
| `Store` | 拥有实例、普通线性内存及宿主定义状态；实例资源保留到 Store 销毁。因此，独立释放某组实例需要让它们有相应的 Store 生命周期。[Store](https://docs.wasmtime.dev/api/wasmtime/struct.Store.html) |
| 已编译 `Module` | 是代码表示，不是运行中的项目状态。克隆仅增加共享引用，可被复用来创建多个实例；保留 Module 不要求保留某个项目 Store。[Module](https://docs.wasmtime.dev/api/wasmtime/struct.Module.html) |
| `Engine` | 提供编译及运行环境，可在进程内共享；所有引用消失后才释放。仅删除最初的 Engine 变量不代表所有使用者已经消失。[Engine](https://docs.wasmtime.dev/api/wasmtime/struct.Engine.html) |
| runtime 进程 | 是上述对象和其他原生状态的容器。进程退出与某一个 Store 被回收是不同操作；Windows 会在进程结束后释放其已提交页的存储，但共享页仍可能由其他使用者或系统缓存持有。[Windows 页状态](https://learn.microsoft.com/en-us/windows/win32/memory/page-state)、[工作集](https://learn.microsoft.com/en-us/windows/win32/memory/working-set) |

**在同一个插件进程内，按项目/模块建立多个 Store、共享 Engine 和已编译 Module，是可行的。**官方示例直接共享 Engine/Module，再分别创建 Store/实例。把这项机制用于“关闭一个项目只销毁该项目 Store”属于 Nanobug 的设计推断，未被实现，也不要求增加每项目进程。[Wasmtime 多线程示例](https://docs.wasmtime.dev/examples-multithreaded-embedding.html)

### guest 释放对象不等于缩小线性内存

`memory.grow` 为现有线性内存增加页，Wasmtime 普通 Memory 接口没有对应的 shrink 操作。把 guest 里的字符串、索引或容器释放给 guest allocator，不应等同于缩小 Memory 或把页退还操作系统。[WebAssembly memory.grow](https://webassembly.github.io/spec/core/exec/instructions.html#exec-memory-grow)、[Wasmtime Memory](https://docs.wasmtime.dev/api/wasmtime/struct.Memory.html#method.grow)

一个可核查的例子是 Rust `wasm32-unknown-unknown` 的默认分配器 dlmalloc：Rust 官方文档说明该 target 使用 dlmalloc；本次读取的 dlmalloc `0.2.14` Wasm 适配器使用 `memory_grow` 获取新页，而系统层的 `free`、`free_part`、`can_release_part` 都返回 false。因此 allocator 内部复用空闲块与向宿主归还整段线性内存是两层动作。这里是该 target/allocator 的例子，不代表 Nanobug 已选择它，也不外推到所有自定义 allocator。[Rust target](https://doc.rust-lang.org/rustc/platform-support/wasm32-unknown-unknown.html)、[dlmalloc Wasm 源码](https://docs.rs/crate/dlmalloc/latest/source/src/wasm.rs)

由此可得：**所有项目关闭但继续保留原来的大实例，不能保证插件进程占用回到小基线。**释放业务对象可能降低有效数据量、让后续任务复用空间，却不必降低线性内存页数或任务管理器中的数值。要获得可预测的实例资源回收，应有能力取消在途执行并销毁对应 Store，之后用缓存的 Module 创建新实例；这是建议，不是“每次关闭都立即销毁”的既定策略。

### OnDemand 与 Pooling 的差异

Wasmtime 默认 `OnDemand` 在实例化时分配资源，在所属 Store 销毁时立即释放实例资源。`Pooling` 则预先建立资源池，Store 销毁后把资源归还池。二者都释放该实例的所有权，但归池不等于释放整个池或退出进程。[InstanceAllocationStrategy](https://docs.wasmtime.dev/api/wasmtime/enum.InstanceAllocationStrategy.html)

Pooling 官方文档明确说明它可能在没有活跃实例时仍保留常驻内存；暖槽、decommit 批处理等配置影响这一行为。所读文档中的 `linear_memory_keep_resident` 仅在 Linux 生效，不能直接作为 Windows 回收方案。[PoolingAllocationConfig](https://docs.wasmtime.dev/api/wasmtime/struct.PoolingAllocationConfig.html)

所读 `52.0.0-dev` 文档提供 `Engine::release_idle_pool_memory`，用于释放未使用槽中保留的常驻内存，同时可能增加下次实例化成本。它不是销毁仍在使用的实例，也不是释放整个 Engine；正式版本是否具有该接口必须在选型时复核。[release_idle_pool_memory](https://docs.wasmtime.dev/api/wasmtime/struct.Engine.html#method.release_idle_pool_memory)

即使采用 OnDemand，实例资源已被释放也不能推导“整个进程 RSS 立刻恰好下降同样字节”：代码缓存、宿主分配、共享资源仍可能存在。ResourceLimiter 本身也不覆盖所有这些分配。具体平台的分配器与 OS 可见回落应通过实测区分，不把 allocator 的释放语义写成固定的任务管理器数字。[ResourceLimiter](https://docs.wasmtime.dev/api/wasmtime/trait.ResourceLimiter.html)

### 观察内存时需要分别记录

| 口径 | 含义及误读 |
| --- | --- |
| 虚拟地址预留 | 先占住地址范围，Windows 的 reserved 页没有关联物理存储；预留很大不能直接当作 RAM 用量。[Page State](https://learn.microsoft.com/en-us/windows/win32/memory/page-state) |
| 已提交内存 | 系统承担相应存储承诺；不表示所有页此时都驻留 RAM。[Allocating Virtual Memory](https://learn.microsoft.com/en-us/windows/win32/memory/allocating-virtual-memory) |
| 工作集/驻留内存 | 当前在物理内存中的相关页，可能因 OS 调页变化；工作集下降不单独证明项目对象已经释放。[Working Set](https://learn.microsoft.com/en-us/windows/win32/memory/working-set) |
| 私有与共享页 | 工作集包含私有与共享数据；共享代码页不能在汇总多个进程物理占用时简单重复相加。[Process Working Set](https://learn.microsoft.com/en-us/windows/win32/procthread/process-working-set) |

“共享页”是 OS 内存统计概念，不表示宿主把不同项目的可变 WASM 堆直接共享。普通 Wasmtime Memory 属于 Store；如果以后引入特殊 SharedMemory，需要另行设计其所有权与同步，不能沿用本节对项目私有 Store 的回收推断。[Memory](https://docs.wasmtime.dev/api/wasmtime/struct.Memory.html)

### 面向 Rust/Vue 场景的生命周期

用户已采纳主架构笔记第 18 节的生命周期方案：项目关闭时释放独占资源，项目仍打开时不因闲置回收，最后资源所有者结束后收尾并退出插件进程。以下运行时机制是实现方向；具体 Store 粒度、收尾期限及分配器尚未冻结，也未实现或验收。

1. **仅安装**：保留磁盘上的包、清单与可再生编译缓存，不因安装而创建全部运行实例。
2. **配置条件首次满足**：启动或复用插件 runtime，复用该进程 Engine；加载编译代码，再为配置选中的模块及相应项目建立 Store。条件满足即放行，不增加“首次实际使用”的激活门槛。文档、项目、面板和后台任务分别记录资源归属。
3. **使用中**：项目私有索引与文档状态归属相应 Store 或显式宿主资源作用域；共享编译代码和只读资源由单独引用计数管理。
4. **关闭 Vue 项目**：先停止新请求、撤销订阅与文件监视，取消并结束在途任务，再释放 Vue 项目作用域及其 Store。关闭项目与仅切走当前标签不同；仍打开的独立文件、面板或其他项目可能保有真实需求。
5. **仍有其他消费者**：保留它们使用的 Store、grammar、共享资源和 runtime。若 Rust 项目仍依赖某个通用插件，关闭 Vue 不应终止那个通用插件的整个进程。
6. **该插件最后一个作用域关闭**：按已确认方案，在没有其他项目、窗口、全局模块或待收尾任务持有该插件时退出进程，磁盘编译缓存保留。用户已经否决按闲置回收，不能重新把空闲时长、面板隐藏或近期调用次数作为实例停用依据；仍打开且保留实例的项目仍是资源所有者。
7. **再次打开**：优先复用编译产物，重新实例化并加载必要项目状态；退出进程后也可从本机磁盘编译缓存恢复，但仍会付出进程启动及实例化成本。

其中引用计数、任务取消、宿主资源释放、原生 LSP 的工作区解绑或退出，都需要 Nanobug 自己实现。Store drop 不会替宿主自动取消所有外部订阅或证明另一个原生进程已经退出。若为了速度永久保留重实例，应明确接受其内存高水位可能保留，不能同时承诺项目关闭后内存必然明显回落。

## 追加调查：多个 JS 规则能否并行

调查日期：2026-11-09。本次查看 QuickJS 官方手册及 rquickjs `0.14.0` 文档；Nanobug 尚未选定 QuickJS/rquickjs，也未实现或测量以下执行器方案。

**多条规则可以同时等待数据，也可以设计成真正同时计算；采用一个公共进程并不限制为一个 JSRuntime。**需要区分三层：原生进程是容器，JSRuntime 是解释器及其对象堆，JSContext 是其中的全局环境。

| 情况 | 核实结果 |
| --- | --- |
| 同一 JSRuntime 内多个 Context | QuickJS 允许多个 Context 共存及共享对象，但一个 runtime 内不支持同时从多线程执行。增加 Context 不会获得多核 JS 计算。[QuickJS：Runtime and contexts](https://bellard.org/quickjs/quickjs.html#Runtime-and-contexts) |
| 多条规则等待宿主异步调用 | rquickjs 的 `futures` 功能可以把 Rust Future 转为 JS Promise，也可以反向转换；AsyncRuntime 提供驱动待处理任务的接口。因此可以让多条规则分别等待文件读取，数据返回后再继续，而不是必须等前一条整个判断结束才启动下一条。[rquickjs 异步机制](https://docs.rs/rquickjs/latest/rquickjs/#optional-features)、[AsyncRuntime](https://docs.rs/rquickjs/latest/rquickjs/struct.AsyncRuntime.html#method.drive) |
| 给同一 runtime 开启 `parallel` | rquickjs 明确通过互斥锁防止多个线程同时在同一 runtime 中运行脚本或创建对象；`parallel` 不会取消这项限制，不能据其名称宣称同一堆中的 JS 自动多核并行。[rquickjs：Runtime and Context](https://docs.rs/rquickjs/latest/rquickjs/#the-runtime-and-context-objects) |
| 多条规则真正同时计算 | 在同一个原生进程中设置多个执行线程，各自拥有独立 JSRuntime，是满足上述约束的候选结构。QuickJS 支持多个 runtime，但它们不能直接交换 JS 对象；输入输出应通过普通数据或消息传递。这是依据官方约束作出的实现推断。[QuickJS](https://bellard.org/quickjs/quickjs.html#Runtime-and-contexts) |

异步等待与 CPU 并行应分别计数。单 runtime 的候选流程是：“规则 A 发起异步读取并挂起 → 规则 B 先做判断或发起另一次读取 → 谁的数据就绪就继续谁”；这个过程中可以有多项未完成判断，但同一时刻仍只有一段 JS 在该 runtime 中执行。宿主必须实际提供非阻塞的异步调用并持续驱动 Promise；仅写上 `async/await` 不会自动安排线程，也不能使一个长时间的同步计算片段让出该 runtime。[rquickjs 运行时锁与 Future/Promise 转换](https://docs.rs/rquickjs/latest/rquickjs/)、[AsyncContext 示例](https://docs.rs/rquickjs/latest/rquickjs/struct.AsyncContext.html#method.async_with)

用户已采纳“不同规则可以并行，但控制总数量”。若选用 QuickJS，同一公共进程内设置有数量上限的执行器、各自拥有独立 JSRuntime 并支持异步等待，是满足该方向的实现候选；单 JSRuntime 搭配多项异步等待可以作为性能对照。多个执行器能让不同规则在不同核上计算，但重复的解释器、脚本状态和调度也有成本；具体数量须按规则工作量、内存及输入延迟测量后决定，本文不预设线程数。多个 JSRuntime 仍在同一个进程故障域内，不改变已经选择的公共规则进程结构；本次采纳不等于选定 QuickJS。

Q28 只约束“同一规则 × 同一项目/文档”最多一项执行和一项最新等待，避免同一来源重复堆积；它不要求不同规则或不同项目彼此串行。跨规则总数量受控、Q29 当前操作优先且其他任务持续推进均已确认；具体计算并行上限、同时等待的宿主请求数、执行器数量与调度算法尚未确定。[主架构笔记：Q28、Q29 与并行采纳记录](2026-11-08-plugin-architecture.md)

## 追加调查：大文件只返回符合条件的部分

调查日期：2026-11-09。核对 Rust std `1.99.1`、Serde 官方示例及 serde_json `1.0.154` 文档；未实现或实测。后续 Q31 已确认源文件总大小不设门槛、宿主筛选与结果分批的方向；以下机制仍是实现候选，不代表选定库、查询语法、页大小或具体资源限制。

**可以不因源文件总大小而拒绝查询，由 Rust 宿主边读边筛选，只把匹配结果交给 JS。**实现基础和边界如下：

- 文本可用 `BufRead::fill_buf` / `consume` 按缓冲块处理；但 `read_line` / `read_until` 会一直向目标缓冲区追加到分隔符或 EOF，因此“逐行”本身不保证小内存，极长的一行仍需处理。这些是同步读取接口，后台执行与取消需另行安排。[Rust BufRead](https://doc.rust-lang.org/std/io/trait.BufRead.html)
- JSON 数组可以通过 `Visitor` / `SeqAccess::next_element` 边反序列化边处理，无须先收集成完整 `Vec`；按记录字段筛选、只留下所需字段，是基于此机制的实现候选。`StreamDeserializer` 迭代的是输入流中多个完整 JSON 值，不能直接当作单个顶层数组的元素迭代器。[Serde 数组示例](https://serde.rs/stream-array.html)、[StreamDeserializer](https://docs.rs/serde_json/latest/serde_json/struct.StreamDeserializer.html)
- **推断：返回少不代表磁盘读取少。**没有索引或已知位置时，首次条件查询可能仍需扫过大部分甚至整个文件；找第一项可能提前结束，确认没有匹配或取齐所有匹配通常需要继续扫描。该边界来自上述顺序读取和逐项解析机制，不能由“流式”推导任意文件都能在 5 秒内查完。
- **待确认的接口设计：**宿主必须明确支持哪些条件，例如文本包含某内容、JSON 记录某字段等于某值，以及返回哪些字段；上述读取/解析 API 本身没有通用查询语言。任意 JS 回调仍需 JS 执行，不能视为 Rust 已原生支持的查询条件。
- **结果分批已确认，具体资源策略待设计：**匹配很多时按批返回；实现还应让消费者处理速度约束生产速度，避免先在宿主或 JS 中攒齐全部结果。单条记录/字段特别大、取消和文件变化等情况也要处理；流式输入不能承诺无限内存，也不能自动保证有界内存。上述 Serde 示例只证明无需保存整个数组，具体上界仍取决于单项表示、解析器状态及结果队列。[Serde 数组示例](https://serde.rs/stream-array.html)

## 追加调查：条件读取的性能、启动影响与格式边界

调查日期：2026-11-09。核对 Rust std `1.99.1`、Serde 官方示例和项目声明的 `toml = "0.8.23"` 对应上游源码；本节没有测试或性能实测。Q30、Q31 已采纳，Q32 的筛选条件仍待确认，以下不冻结支持格式清单。

**库机制：读取字节、读取文本、按字段查询是三层能力。** 普通可访问文件可按字节读取，不取决于扩展名；把字节解释成文本需要正确编码，Rust `read_to_string` 要求有效 UTF-8，其他编码需要相应解码。读取成功本身不代表能够理解文件内的字段。[Rust Read](https://doc.rust-lang.org/std/io/trait.Read.html)

**库机制：结构化查询取决于格式和解析器。** Serde 的 JSON 数组示例允许逐项处理而不收集完整数组；项目当前声明的 TOML 版本则通过 `from_str(&str)` / `Deserializer::new(&str)` 接收文档文本，并在调用 Visitor 前交给 `toml_edit` 解析。不能把 JSON 的逐项处理能力推广成所有格式都有增量解析，也不能认为 TOML 结果分批返回就意味着解析时不保留完整输入。具体格式查询需逐个接入、核实实现。[Serde 数组示例](https://serde.rs/stream-array.html)、[toml 0.8.23 上游源码](https://github.com/toml-rs/toml/blob/toml-v0.8.23/crates/toml/src/de.rs)、[本项目 Cargo.toml](../../Cargo.toml)

**设计推断：条件读取是否昂贵，取决于扫描量、解析成本、条件复杂度、重复查询与并发数量。** 流式处理和分批返回可以减少同时保存的输入/结果及传给 JS 的数据，但仍可能扫描全文件、逐项解析和比较；它们不自动保证低 CPU、少磁盘读取或更短耗时。Serde 示例本身也要遍历全部元素才能求最大值。沿用 Q31 的整次判断 5 秒预算，未查完应表达为未完成，不能把超时当作没有匹配。[Serde 数组示例](https://serde.rs/stream-array.html)、[Q31 决策](2026-11-08-plugin-architecture.md)

**设计推断：按 Q30，条件查询在核心界面和基本操作就绪后才开始，不成为首次就绪必须等待的步骤。** 启动后的查询仍与编辑操作争用 CPU、磁盘和内存，可能影响交互或插件首次可用时间；后台或异步执行不能证明零影响。已确认的并发控制和判断预算需要落实到调度及取消，实际效果仍须测量。[Q30 与并行决策](2026-11-08-plugin-architecture.md)

## 追加调查：Lua 条件如何处理宿主不认识的格式

调查日期：2026-11-09。仅核对 Neovim 与 Lua 官方读取、解析和异步语义；未执行用户配置、测试性能或新增实现。本节不冻结 Nanobug 的 Q32 方案，lazy.nvim 的具体条件求值时机另行核对。

- **读取不要求宿主认识格式。** Lua 可以用 `vim.fn.readfile(path)` 取得行列表，或用 `io.open(path)` 与 `file:read(...)` 取得内容；结果仍是文本/字节，没有自动成为配置字段。`readfile` 也不自动识别字符编码。[Neovim readfile](https://neovim.io/doc/user/vimfn/#readfile())、[Lua I/O](https://www.lua.org/manual/5.1/manual.html#5.7)
- **简单格式由脚本解释。** 若自定义文件明确规定每行是 `engine=nova` 这种键值，作者可用 Lua 字符串处理、`string.match` 等提取值，再比较是否匹配。此办法依赖作者掌握格式规则，不代表简单字符串匹配就能正确解析任意嵌套配置。[Lua 字符串库](https://www.lua.org/manual/5.1/manual.html#5.4)
- **JSON 有现成入口，其他格式可以补解析器。** `vim.json.decode(text)` 将 JSON 文本解码成 Lua 对象；复杂自定义格式可用作者自己的 Lua 模块或额外解析器，再通过 `require` 加载。模块必须能被相应模块搜索路径找到；`require` 是模块加载机制，不会自动理解任意配置文件。[Neovim JSON](https://neovim.io/doc/user/lua/#vim.json.decode())、[Neovim 模块加载](https://neovim.io/doc/user/lua/#lua-module-load)
- **普通调用不会自动在后台执行。** 上述直接读取、字符串处理和解析调用是同步工作。Neovim 提供显式线程 API，但线程使用独立 Lua 状态；`vim.schedule` 只是安排主事件循环稍后调用函数。[Lua I/O](https://www.lua.org/manual/5.1/manual.html#5.7)、[Neovim 线程与调度](https://neovim.io/doc/user/lua/#lua-loop-threading)
- **异步结果要有专门的数据流。** `vim.uv` 的文件操作可通过回调异步执行；提供回调时立即返回的是请求等信息，读取结果稍后交给回调。由此可知，若调用方要求当前函数直接返回布尔值，稍后在回调中 `return true` 不会成为外层函数此次返回值；需要预先取得结果、缓存，或由调用方另行提供异步协议。[Neovim luv 同步与异步](https://neovim.io/doc/user/luvref/)、[Lua return 语义](https://www.lua.org/manual/5.1/manual.html#2.4.4)

**便于解释的例子：** 公司自定义 `.companyconf` 写着 `engine=nova`。Neovim 负责让 Lua 读到这段文本，脚本作者负责教它识别 `engine`，最后条件函数判断值是否等于 `nova`。若文件改成复杂语法，就换合适的解析器；宿主无需预先内置公司格式，但必须有人提供理解该格式的代码。
