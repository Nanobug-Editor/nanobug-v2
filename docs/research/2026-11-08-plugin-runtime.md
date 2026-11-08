# 插件运行时机制研究

调查日期：2026-11-08。范围：Nanobug V2 的 WASM 加载、调度、资源约束及 JS 启用规则。本文记录一手资料和候选设计，未进行 Nanobug 插件性能实测，未冻结选型或数值预算。

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

推荐把职责集中到少量深模块中：配置管理给出选中的模块，调用方提交某版本任务或明确关闭，由生命周期管理隐藏去重启动、依赖加载、超时与重建。用户在第二轮明确配置条件满足即放行，否决闲置回收；不能再叠加“首次实际使用才激活”的判断。条件变为不满足时的停用与资源释放仍待定义。

候选规则：

1. 编辑器可编辑后才开展普通插件激活；安装时建立清单索引，运行时不遍历全部包、不解析全部 WASM、不同步执行全部 JS。具体首屏可用能力仍需作为产品决策确认。
2. WASM 编译与进程启动都设并发上限。即使移出 UI 线程，几十个插件同时抢占 CPU/磁盘仍会影响输入延迟。
3. UI 与重型业务不共享必须长时间持有的 Store 或全局锁；前台任务有独立调度份额，后台任务可取消或被暂停。线程数与优先级必须纳入多插件场景实测。
4. 未变化的插件界面复用宿主已经接收的绘制数据；插件只在状态变化时提交批次。宿主不在每帧同步询问插件，也不以逐字符调用作为跨进程粒度。
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

候选规则执行接口只接收宿主缓存的上下文，返回模块选择；不开放 `std`/`os`、任意导入、网络或文件扫描。执行放在 UI 线程之外，限制单次时间、内存及输入规模，按依赖变化重评估。脚本出错保留上一有效配置，复用已加载函数但不永久缓存旧上下文结果。这些是宿主需要实现的规则，不能单靠引擎名称获得。[QuickJS 标准库与中断机制](https://bellard.org/quickjs/quickjs.html)、[需求总结：JS 模块启用规则](../需求总结.md)

用户在 Q5 解释后已确定：所有插件共用一个独立规则判断进程执行 JS 配置。这样无需先启动各插件 runtime 才能判断模块条件，并可单独终止/恢复规则执行环境。进程启动阶段、上下文同步、每条规则的预算和故障后的重新评估协议仍需设计；此确认不包括共享 Tree-sitter 解析进程。

## 高可用需要补齐的宿主行为

以下是结合“WASM 约束只覆盖部分资源”的事实提出的候选协议规则，不是对运行时现有自动能力的描述。[ResourceLimiter](https://docs.wasmtime.dev/api/wasmtime/trait.ResourceLimiter.html)、[阻塞宿主调用限制](https://docs.wasmtime.dev/api/wasmtime/struct.Config.html#method.epoch_interruption)

| 故障 | 建议处理 |
| --- | --- |
| 超时或 Wasm trap | 标记本次任务失败；需要时重建模块 Store；保留核心编辑与最后有效界面。 |
| runtime 进程退出 | 由宿主释放该进程拥有的资源与在途请求，递增会话代次，有限次数退避重启；连续失败后停用并给出可定位的错误。 |
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
- 插件失败时可接受的状态丢失范围与自动重试次数。
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

### 面向 Rust/Vue 场景的候选生命周期

下面只说明怎样利用上述事实实现用户期望，不是已经确认的自动回收时间或保留策略。

1. **仅安装**：保留磁盘上的包、清单与可再生编译缓存，不因安装而创建全部运行实例。
2. **配置条件首次满足**：启动或复用插件 runtime，复用该进程 Engine；加载编译代码，再为配置选中的模块及相应项目建立 Store。条件满足即放行，不增加“首次实际使用”的激活门槛。文档、项目、面板和后台任务分别记录资源归属。
3. **使用中**：项目私有索引与文档状态归属相应 Store 或显式宿主资源作用域；共享编译代码和只读资源由单独引用计数管理。
4. **关闭 Vue 项目**：先停止新请求、撤销订阅与文件监视，取消并结束在途任务，再释放 Vue 项目作用域及其 Store。关闭项目与仅切走当前标签不同；仍打开的独立文件、面板或其他项目可能保有真实需求。
5. **仍有其他消费者**：保留它们使用的 Store、grammar、共享资源和 runtime。若 Rust 项目仍依赖某个通用插件，关闭 Vue 不应终止那个通用插件的整个进程。
6. **该插件最后一个作用域关闭**：是否退出进程仍是待确认策略。主架构笔记第 18 节当前建议在没有其他项目、窗口、全局模块或待收尾任务持有该插件时退出进程，磁盘编译缓存保留。用户已经否决按闲置回收，不能重新把空闲时长、面板隐藏或近期调用次数作为实例停用依据；仍打开的项目按候选规则保留实例时，仍是资源所有者。
7. **再次打开**：优先复用编译产物，重新实例化并加载必要项目状态；退出进程后也可从本机磁盘编译缓存恢复，但仍会付出进程启动及实例化成本。

其中引用计数、任务取消、宿主资源释放、原生 LSP 的工作区解绑或退出，都需要 Nanobug 自己实现。Store drop 不会替宿主自动取消所有外部订阅或证明另一个原生进程已经退出。若为了速度永久保留重实例，应明确接受其内存高水位可能保留，不能同时承诺项目关闭后内存必然明显回落。
