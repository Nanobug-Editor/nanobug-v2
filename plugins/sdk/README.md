# Nanobug 插件 SDK

插件以 ZIP 交付，根目录使用 `nanobug-plugin.toml`。清单的规范名称和 ID 共同标识插件。业务模块使用 WASM，激活条件可以使用单独的 JS 文件。当前公开 API 标记为 `1.0`。UI SDK 通过单独的实验契约启用。

## 在 Windows 测试示例

在工作区根目录执行以下命令。发布目录里的编辑器和插件运行程序需要放在一起。

```powershell
rustup target add wasm32-unknown-unknown
& .\Build.ps1
cargo build --locked --offline --release --target wasm32-unknown-unknown -p nanobug-plugin-sdk --example workspace-panel
$demo = Join-Path $PWD 'test-output/workspace-panel'
New-Item -ItemType Directory -Path $demo -Force | Out-Null
Copy-Item plugins/sdk/examples/nanobug-plugin.toml,plugins/sdk/examples/detect.js,target/wasm32-unknown-unknown/release/examples/workspace_panel.wasm -Destination $demo
& .\dist\Nanobug.exe --plugin-dev-profile (Join-Path $PWD 'test-output/dev-profile') $PWD
```

按 Space → Shift+P，进入 **3 Local ZIP**，选 **Load local build output…** 并输入示例目录。确认权限后，在 **Installed** 打开插件详情，进入 **Modules**，选中当前项目的 `notes`。打开的项目含 `README.md` 时规则放行。若要用 ZIP 安装，把示例目录里的三个文件放在 ZIP 根目录。

字符网格示例使用同一个入口。构建 `workspace-grid`，把 `examples/workspace-grid.toml` 复制到新示例目录并命名为 `nanobug-plugin.toml`，再放入 `detect.js` 和构建所得的 `workspace_grid.wasm`。两种面板都能在宿主输入区打字并保存草稿。

开发环境有自己的插件安装、配置和草稿。上面的 `test-output/dev-profile` 已被 Git 忽略，清理 Cargo 产物后仍可继续使用。也可用 `--plugin-dev-profile <目录>` 指定其他开发配置。**Settings** 的 **All projects** 允许为本地构建配置 Cargo 项目，手动构建或开启自动构建。编译目标为 `wasm32-unknown-unknown`，产物文件名需要与清单中的 WASM 文件名对应。自动构建关注 `Cargo.toml`、`src` 和 `build.rs`。编辑器对开发环境中打开的项目仍使用同一份项目文件。

用 `--plugin-profile <目录>` 可选择普通插件配置。用 `--no-plugins` 启动时，管理面板仍可查看安装记录。测试市场读取配置目录下的 `marketplace.json`，相对 ZIP 路径从该目录解析。

```json
[
  {
    "identity": { "name": "Example", "id": "example" },
    "version": "1.0.0",
    "description": "A local test package",
    "zip": "example.zip"
  }
]
```

## 调用与数据

- Rust 插件用 `export_plugin!(handler)` 导出入口。回调接收 `Input`，返回 `Output`。宿主核对模块导出和导入。安装流程在发布安装记录前检查 WASM 并准备编译缓存。
- 规则是一个异步 JS 函数体，最终返回布尔值。可以通过 `host.exists`、`host.readFile`、`host.readBytes`、`host.scanText`、`host.readDocument`、`host.list` 查询已授权数据。分段读取返回下一位置及是否结束。`readFile` 和 `scanText` 使用 UTF-8，二进制文件可分段用 `readBytes`。文件接口读取磁盘内容，文档接口读取宿主正文。
- 激活规则需要读取项目外文件时，在清单顶层声明 `permissions = ["read_rule_paths"]` 和 `rule_paths`。例如 Windows 本地包可以填写 `rule_paths = ['C:\LocalFixture\status.txt']`。最多列出四个准确的绝对文件路径。JS 调用时传入与清单相同的路径字符串，可查询存在、读取文本或字节、扫描文本。目录列出、正文、写入和业务 WASM 均使用各自的权限。编辑器先显示清单中的路径，用户确认后才访问这些路径并在本机保存授权。文件更新、创建和删除会触发重算。父目录被替换后，可重新安装当前版本并确认路径来恢复授权。规则文件查询拒绝路径中的 `..`、符号链接和 Windows 目录联接，并支持获准的网络共享文件。主动联网接口由业务模块的权限控制。
- 文档位置使用 UTF-8 字节偏移。每次打开和每次正文修改都有独立的标识。编辑结果需要使用同一次输入里的版本。宿主核对整个编辑组，再写入正文。未打开的 UTF-8 项目文件可以用 `ReadProjectText` 读取，显式命令将返回的路径和版本放入 `project_file_edits`。提议修改未打开的文件需要 `read_project` 和 `edit_document` 权限。同一预览可包含打开和未打开的文件。确认后，未打开的文件会成为未保存的缓冲区。写盘仍由用户保存。
- 自动刷新交互模块时，编辑器可以在内部只发送正文变动。运行进程缺少旧版本会请求补发正文，插件回调始终收到原格式的 `Input`。
- 常规设置由清单声明。自定义设置界面使用 `settings_ui` 模块与 `StageSettings` 准备草稿，按宿主设置页的 Apply 后生效。共享项目设置保存到 `.nb/plugins.json`，仅本机的项目设置保存在当前配置环境。
- 模块在 `Open` 或 `SettingsChanged` 无法使用已保存设置时，返回 `Output.settings_error` 说明原因。宿主保留已保存的值，并在设置页提供重试和恢复旧值的入口。
- 命令清单可通过 `contexts` 指定 `editor_normal`、`editor_visual` 和 `explorer`。省略时在这些位置都可用，输入文字和输入法时暂停插件快捷键。用户映射随当前本机配置环境保存。
- 设置等价改名时可用 `renamed_from` 指出同类型的旧键。宿主读取旧值，保存新键需先在设置页准备修改再按 Apply。旧键和无法转换的值会保留，值类型或保存范围发生变化时在设置页提示。
- 工具和凭据等操作使用 `Output.services`，回调通过 `ServiceResult` 返回。工具需要插件安装权限和项目执行许可。可选协作只访问对方已声明且已经就绪的服务。
- 声明 `credential_events = true` 的模块会在自己的凭据变更后收到 `CredentialChanged`。通知仅含设置键和修订，使用 `ReadCredential` 读取新值。处理该服务回复时可通过 `Output.credential_rejected` 报告认证失败，宿主显示固定提示并保留新值。

## 文档语言功能

- 模块可以声明 `provides = ["document", "complete", "diagnose"]`，并用 `document_extensions = ["rs"]` 限定语言入口。JS 的 `host.documentPath()` 依赖文件名，正文修改时可以复用规则结果。
- `document` 提供者收到 `Open` 和 `DocumentChanged`，返回 `Output.document_features`。高亮使用主题角色，折叠返回 UTF-8 范围，大纲返回名称和跳转字节。宿主按文档打开代次、修订和运行代次检查结果。声明该能力需要 `read_document`。
- `Output.completion_items` 可以提供标签和单个替换范围，范围需包含请求的光标位置。原有 `Output.completions` 仍按字符串插入。
- `definition` 由项目授权的原生语言服务处理。宿主用 `Output.definitions` 传递目标文件及从零开始的行号和 UTF-16 列号。
- Rust 模块可以声明 `rust_analyzer = true` 和 `document_extensions = ["rs"]`，将已声明的补全、诊断和定义跳转交给宿主管理的 rust-analyzer。项目授权执行后，宿主同步已打开的 Rust 缓冲区，并仅显示与当前文档修订匹配的实时诊断。普通 WASM 事件仍可提供语法和基础补全。
- `QuerySyntax` 在公共解析进程中对当前正文执行 Tree-sitter 查询，返回捕获范围和语法错误。查询和旧的 `ParseDocument` 共用文档语法树。使用 `await_service = true` 及一条服务请求，`ServiceResult` 的输出将完成原调用，受原调用的截止时间和文档版本约束。
- `ReadProjectAncestors` 使用相同的等待方式，从当前文档目录向上读取指定文件，边界为打开的项目根，要求 `read_project`。返回按距离排序的项目相对路径和 UTF-8 正文。
- `format` 提供者收到 `Format` 后，可以用 `Output.edits` 返回文本修改，或用 `Output.format_error` 报告失败。保存到新路径时，事件的可选 `path` 字段会提供目标，但编辑范围仍以当前缓冲区为准。
- Rust 格式化提供者可以在等待式 `Format` 调用中请求 `Service::Rustfmt`。宿主要求读取和编辑文档、运行本机工具及当前项目执行权限，把当前正文通过标准输入传递给 rustfmt，并以字符串返回标准输出。

## Canvas 自绘

- `Canvas.draw` 按顺序描述一帧。坐标使用逻辑像素。`Viewport` 提供区域尺寸、设备缩放和布局编号。依赖区域尺寸的画面应带上该编号。标准文本输入通过 `Canvas.input` 放入宿主字符网格。点击定位、拖动或用 Shift 加方向键选字、输入法选词和草稿均由宿主处理。绘制、选区和全局光标共用同一份字符位置。
- 自绘文本框可用 `Canvas.text_input` 指定精确矩形、字体、字号和颜色。插件仍可自绘背景、边框和其他内容。宿主用同一份原生排版处理字形、命中、选区、光标及输入法位置，并自动应用全局光标及文字波纹。初始文字可从 `GridSurface.input` 给出，之后的确认输入保存在宿主。`Canvas.input` 与 `Canvas.text_input` 每幅画面择一使用。
- 需要更新部分画面时，先提交带有非零 `Canvas.sequence` 的完整帧，并使用宿主当前的 `Viewport.id`。随后的调用从 `Input.canvas_sync` 获取已准备成功的序号、宿主基线编号、视口和运行代次。`Output.canvas_delta` 回传这些标识，`base` 用已确认序号，`sequence` 加一。同一回复只能提交完整帧或增量之一。缺帧或资源无效时，宿主保留最后一幅有效画面并发出 `CanvasResync`。收到该事件或新视口后，重新提交完整帧，等待宿主确认后再送增量。
- `Canvas.interaction` 可声明鼠标样式和按下、松开、移动、滚动回调。每种事件取最后一个命中的声明，宿主文字框优先。现有 `Canvas.hits` 左键行为仍可使用。点击自绘区后，可用 `keys` 中的按键和修饰键组合操作它。全局快捷键和宿主文字输入优先，Esc 或点击面板外会交还焦点。
- 回调继续使用 `PluginEvent::Ui`，`action` 是声明中的名字。`argument.event` 给出回调类型。鼠标事件包含相对坐标 `x` 和 `y`，滚动另含逻辑像素 `delta_x`、`delta_y`。键盘事件含 `key`、`held` 和修饰键。`next_frame` 每个显示中的有效画面最多派发一次，移动、滚动和帧回调在忙时可能合并或跳过。提交正文和运行外部命令仍需明确的用户操作。自绘示例用鼠标或 F2 显示回调状态。
- 路径的 `style` 可选择填充规则或指定描边的线宽、起止端点、转角、尖角限制与曲线误差。旧的 `stroke` 字段仍可指定默认描边的宽度。`Group` 支持裁剪、批次、相对或绝对偏移和嵌套透明度。位图和彩色字形沿用 GPUI 的元素透明度。
- `Draw::Text.layout` 可指定对齐、对齐宽度和最多绘制的行数。系统文字、单字形及其测量共用 `font_options`，可指定字重、倾斜、OpenType 特性和回退字体。SVG 支持围绕中心的缩放和旋转，也可指定 `[a,b,c,d,tx,ty]` 仿射变换。旋转用弧度，平移用逻辑像素。
- 单行文字需要控制下划线时，使用 `Draw::ShapedLine`（JSON `op: "shaped_line"`）。沿用现有的 `ui-2026-11-12-1` 实验契约。收到 `Input.viewport.shaped_line == true` 后才提交；视口未到达或字段为假时，使用 `Draw::Text` 绘制普通文字。新画面可使用视口的 `id`，并在收到 `Layout` 时按新尺寸重绘。
- `Viewport.appearance` 提供编辑器语言、默认字号、界面缩放和主题色。新宿主还提供 `base_font_size`、内置 `font`、当前 `theme` 及边框和错误色；旧宿主可省略它们。`font_size` 是乘过用户缩放后的逻辑像素字号，`Viewport.scale` 仍表示系统设备缩放。`Language::text` 按每条业务文案选择译文，缺译文时使用英文。字体名省略时使用编辑器默认字体。已打开的界面通过布局事件接收新外观，显式设置的局部字体和颜色仍由插件决定。
- 自带的 TTF、OTF 或 TTC 可用 `Draw::PackageText` 绘制。提供包内路径、字形所在的 face 编号、字号、单行文本和基线。`MeasureText` 的 `package_font` 选项使用同样的字体和布局，返回宽度、纵向尺寸和 UTF-8 字素位置。系统字体行和包内字体行可在同一个请求中测量。包内字体准备完成后只保留可绘制的轮廓。
- 当 `Viewport.apple_surface` 为真时，可以用 `Draw::AppleSurface` 提交包内的全范围 NV12 数据。文件由紧密排列的 Y 行和交错 CbCr 行组成，宽高均为 2 至 2048 范围内的偶数。宿主创建 IOSurface 支持的 CoreVideo 缓冲区，再交给原生 GPUI。其他平台可用 `Draw::Image` 提供同一内容的位图版本。[Apple CoreVideo 像素缓冲区](https://developer.apple.com/documentation/corevideo/cvpixelbuffer?language=objc)

### 单行文字的下划线

`ShapedLine` 接受 `origin`、`text`、`size`、`line_height`、`runs`，以及可选的 `layout`（对齐方式和对齐宽度）。单行最多 512 个 UTF-8 字节和 16 个样式段。先给 `runs` 中需要下划线的文字段设置 `underline`，再用可选的 `underlines` 指定调整规则。

```json
{
  "op": "shaped_line",
  "origin": [12, 8],
  "text": "中A",
  "size": 15,
  "line_height": 22,
  "runs": [{"length": 4, "color": 4294967295, "underline": {"thickness": 1}}],
  "underlines": [{"range": [0, 4], "style": {"thickness": 2, "wavy": true}, "exclusions": [[6, 9]]}],
  "underline_tag": 17
}
```

`range` 使用原文字的 UTF-8 字节偏移，右端点不包含在内。每条规则匹配 GPUI 排版后生成的整个下划线段，相同样式可能合并成一段。可用 `skip: true` 跳过这一段，或用 `style` 改变线宽、颜色和波浪样式。替换样式时省略 `color` 会保留原色。`exclusions` 从这一段下划线的起点向右量，以逻辑像素留出空白。每行最多 16 条按字节范围排序的互不重叠规则，每条最多 16 个留空区间。

需要坐标时，同时填写 `underline_tag` 和 `Canvas.interaction.next_frame`。每幅画面（含所有嵌套组）可填写最多 16 个互不重复的标签。宿主在实际绘制后通过 `PluginEvent::Ui` 发送 `argument.underlines`，其内容可解码成 `Vec<LineUnderlines>`。每个标签附带原生的 `range`、相对画布的 `origin`、`width`、`thickness`、`color` 和 `wavy`，坐标包含组偏移，颜色是 0 至 1 的 RGBA。数据取自样式覆盖、裁剪和设备像素对齐之前。根据坐标补画的内容放在下一幅画面里。同一画面最多回调一次，宿主忙时可跳过回调。仍需获取坐标时，在当前视口提交一幅新画面再次请求。

## 当前测试范围

Windows 已用原生 GPUI 路径对照验证描边与填充，也用实际安装的 WASM 和包内字体验证了测量、轮廓准备和项目关闭后的释放。Apple 端还需实际显示验证。语言结果与格式化使用有界的打开文档快照。`ReadProjectText` 单文件上限为 120 KiB。JS 规则的分段读取仍支持更大的文件。

编辑器提供 `:complete`、`:diagnose`、`:outline`、`:fold`、`:plugin <命令 ID>`、`:format`、`:formatcancel` 和 `:undo-plugin`。视觉选区进入 `:format` 时，宿主会保留选区并检查结果。保存时格式化在设置里开启，按 Apply 生效。

## 原生工具

工具需要 `run_tool` 权限，下载额外需要 `network`。每个项目还需要在 **Modules** 中允许执行。工具使用编辑器当前的系统权限。

```toml
[settings.helper-path]
title = "Existing helper path"
default = ""

[tools.helper]
path_setting = "helper-path"

[tools.helper.downloads.windows-x86_64]
url = "https://example.org/helper.zip"
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
executable = "bin/helper.exe"
```

填写实际 ZIP 的 SHA-256。平台键为操作系统与架构，例如 `windows-x86_64`。用户指定的已有工具路径优先，其次是工具声明的随包 `executable`，最后选择当前平台的下载项。宿主调用系统 HTTPS 下载程序，校验并解包到编辑器配置目录。同一 SHA-256 可以复用缓存。ZIP 上限为 128 MiB。下载缓存的软额度为 512 MiB 或 32 个归档，超出时优先清理最久未使用的闲置归档。工具启动和运行期间保留整份归档及其依赖。

一次性命令通过 `RunTool` 提交。宿主保存执行记录，插件详情的 **External operations** 可查看最近的工具和文件操作结果。中断后无法确认的记录显示为「结果待确认」。工具退出码只描述进程结果，外部副作用需要按对应业务核对。

可从快照安全重建的工具可以在 `[tools.helper]` 内声明 `background = { args = ["--nanobug-stdio"] }`。插件在用户命令中发送 `StartBackgroundTool`。工具逐行从标准输入读取 JSON 格式的 `ToolSnapshot`，处理后向标准输出写一条 `ToolSnapshotReply`。每条回复必须带上对应的 `revision`，可以附带少量可重建的 `result`。日志应使用标准错误流。

宿主向持有 `read_document` 权限的服务发送该项目的当前正文，包括未保存的修改。单次同步最多 16 份文档和 120 KiB 文本，`omitted_documents` 指示未送出的快照数量。插件用 `BackgroundToolStatus` 取得当前状态和仍有效的结果。工具必须先确认一次快照才会自动恢复，恢复时只发送最新快照。Windows 发现进程持续超出 384 MiB 私有内存时会先通过 `discard_rebuildable_cache` 请求释放可重建缓存，再暂停超额服务。用户可以从模块状态发起重试。
