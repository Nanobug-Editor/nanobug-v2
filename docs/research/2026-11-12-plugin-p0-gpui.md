# GPUI 0.3.10 插件绘图能力清单

参照 `gpui-pre` 与 `gpui-pre-platform` 0.3.10。以下覆盖 Canvas 回调可用的公开绘制入口及实现所需的准备、资源和交互。P0 只验证一个真实插件的字符网格和宿主输入。P3 需要逐行补上原生 Canvas 与 SDK 同场景的对照。

Canvas 先用样式申请布局，然后按顺序执行 `prepaint`、`paint`。宿主执行这些阶段并持有原生对象。插件给宿主提交可校验的数据，准备结果以宿主句柄和布局代次引用。[Canvas 固定版源码](https://docs.rs/crate/gpui-pre/0.3.10/source/src/elements/canvas.rs)

| 公开入口 | 对应的 SDK 能力 | P0 证据或后续对照 |
| --- | --- | --- |
| `paint_quad`，`fill`、`outline`、`quad` | 矩形、圆角、各边宽度、边框样式与颜色 | 验证纯色矩形。待对照圆角、边框、渐变与图案 |
| `Background`、`solid_background`、`linear_gradient`、`pattern_slash`、`checkerboard` | 纯色、原生线性渐变、斜线、棋盘填充和原生颜色空间选项 | 待对照各填充在矩形及路径中的原生行为 |
| `paint_path`、`PathBuilder` | 路径填充与描边，按原生支持的曲线、闭合、笔划和变换传数据 | 待对照路径形状、填充/描边和坐标变换 |
| `paint_drop_shadows` | 外阴影参数、裁剪、圆角 | 待对照阴影、多个阴影及边缘 |
| `paint_inset_shadows` | 内阴影参数及边界 | 待对照内外阴影叠加顺序 |
| `paint_glyph` | 通过宿主文字排版或受管字形引用绘制 | 验证宿主排版的文字。待对照单字形入口、字体回退及各种文字布局 |
| `paint_emoji` | 彩色字形，通过宿主字体/排版引用 | 已在真实 P0 网格接收和绘制 Emoji。待对照平台字体和原生彩色字形效果 |
| `paint_underline` | 直线、波浪等原生下划线样式及偏移 | 待对照宽度、波浪和缩放 |
| `paint_underline_with_exclusions` | 指定避让区间的下划线 | 待对照多个区间、裁剪和文字重排 |
| `paint_strikethrough` | 删除线样式及位置 | 待对照文字布局与顺序 |
| `shape_line`、`shape_line_by_hash`、`shape_text`，排版结果的 `paint` / `paint_background` / `paint_with_underline_handler` | 文字测量、字体范围、单行与换行排版、选择/背景和文字装饰。宿主保留原生排版结果 | P0 复用 `TextSpacing` 与 `Panel`。待对照多行、测量结果句柄、下划线策略和选区 |
| `paint_svg` | 受管 SVG、单色绘制、尺寸及其原生 `Transformation` | 待对照 SVG 资源、旋转、缩放、平移及透明度 |
| `paint_image` | 受管图像、源/目标范围、圆角、帧序号和灰度 | 待对照静态图、动画帧、裁剪和图像释放 |
| `paint_surface` | 平台专属表面资源句柄，宿主负责持有与释放 | macOS/iOS 条件编译，使用原生 `CVPixelBuffer`。需在相应平台验证桥接，Windows 不声明可用 |
| `paint_layer` | 限定区域和顺序的绘制批次 | 待对照重叠、顺序和嵌套；依照原生批次语义 |
| `with_content_mask`、`with_element_offset`、`with_absolute_element_offset`、`with_text_style` | 裁剪、相对/绝对偏移及局部文字样式；透明度由宿主按原生样式与原语传递 | 待对照组合变换和裁剪。`with_element_opacity` 在固定版本中是内部入口 |

| 配套能力 | 对应的 SDK 能力 | P0 证据或后续对照 |
| --- | --- | --- |
| `request_layout`、`request_measured_layout`、`layout_bounds`、Canvas 两阶段回调 | 受管区域、尺寸和准备结果，同一有效代次的绘制数据 | P0 固定两行网格区域。待对照通用布局、重排与资源尚未就绪 |
| `use_asset`、`with_image_cache`、`drop_image`、字体选择 | 宿主管图像与字体，句柄归属和有效期可验证 | 待对照资源请求、引用、重启失效和关闭时释放 |
| `insert_hitbox`、`on_mouse_event`、`on_key_event`、`set_cursor_style` | 先准备命中，再注册对应交互；区域事件经宿主转交 | P0 点按聚焦和键盘经过工作区。待对照任意区域、命中和旧代次输入 |
| `focus`、`handle_input`、`on_next_frame` | 焦点、文本/IME、光标和下一帧通知 | P0 用全局光标及原生输入完成中文候选验证。待对照多控件、滚动、缩放、选区和各平台 |

核对入口见 [Window 固定版源码](https://docs.rs/crate/gpui-pre/0.3.10/source/src/window.rs)、[颜色与填充](https://docs.rs/crate/gpui-pre/0.3.10/source/src/color.rs)、[路径构造](https://docs.rs/crate/gpui-pre/0.3.10/source/src/path_builder.rs)和[本机网格绘制](../../src/grid/render/panel.rs)。发布声明以各平台逐项对照结果为准。

## Windows 试用版补充

| 范围 | 已验证的行为 | 仍需实测 |
| --- | --- | --- |
| 路径 | 起止端点、转角和非零填充规则与直接原生构造的边界和顶点一致。组合场景对照了裁剪、分批和变换后的虚线描边 | 其他曲线、填充规则及平台的像素矩阵 |
| 透明度与偏移 | 组合场景对照了相对和绝对偏移、矩形裁剪、原生绘制批次、裁切 PNG、彩色 Emoji 及变换后的 SVG 透明度 | 其他平台和缩放下的原生画面对照 |
| 系统文字与图像 | DirectWrite 单字形、彩色 Emoji、字体回退、字重与斜体、OpenType 特性、文字对齐和行数。测量的位置与原生缓存排版一致。对照了同一 GIF 的两帧及灰度 | 更广的字体回退、字体渲染、复杂文字和图像编码矩阵 |
| 包内字体 | 真实 WASM ZIP 中的字体可异步测量并生成原生路径。验证字体数据在准备后释放，项目关闭时画面资源释放 | 其他字体、复杂文字与宿主编辑框的画面对照 |
| 平台专属画面 | Apple NV12 路径创建只读 IOSurface 缓冲区。完整 Apple ARM64 编辑器已在隔离的依赖目录生成 GPUI Metal 源并通过类型检查 | 在 Apple 主机运行行复制测试并显示原生画面 |
| 网格与自绘样例 | 两个 WASM 样例均通过 Windows 前台输入、拼音选词及选区验证。自绘样例和此前构建的 ZIP 均经过实际插件进程加载 | 更多缩放、主题、字体、光标效果及平台的交互对照 |

### Windows DirectX 组合场景

测试把同一画面交给固定版 GPUI 和真实安装的 WASM 插件。在同一个不显示的原生窗口内用 DirectX 读取两次 RGBA，截取 300 × 260 逻辑像素。场景包含圆角、不等宽虚线边框、Oklab 和 sRGB 渐变、斜线和棋盘、内外阴影、相对及绝对偏移、裁剪、绘制批次、变换后的虚线曲线、偶奇填充的椭圆弧和二次曲线、文字背景和换行、下划线避让、删除线以及两层透明度下的裁切 PNG。另有两种 SVG 变换、直接绘制的单字形和半透明彩色 Emoji，以及 GIF 彩色和灰度帧。测试检查这些图形实际可见，并比较系统字体测量和字素边界。分别解码的两组 GIF 帧逐字节比较后共用图集位置，控制 DirectX 无填充图集在边缘采样时的相邻像素影响。[场景与测试](../../src/grid/workspace/plugin_manager/canvas/parity.rs)

Windows 当前显示器采用 150% 缩放。175,500 个设备像素中有 175,491 个完全一致，另外 9 个位于变换路径边缘，每个 RGBA 通道最多相差 1。原生参考在路径剖分前平移，宿主在剖分后加上画面原点，边缘有浮点舍入差异。测试最多容许 64 个路径区域像素出现这一幅度的差异，其他区域要求完全一致。另在同一个原生窗口中指定 GPUI 的 100%、125%、200% 比例，对比分别有 78,000、121,875、312,000 个完全一致的像素。125% 对比发现并修正了 SDK 的 SVG 旋转中心与原生设备像素对齐不一致。

```powershell
cargo build --locked --offline -p nanobug-plugin-runtime
cargo test --locked --offline -p nanobug --bin nanobug sdk_canvas_matches_native_directx_pixels -- --ignored --nocapture
```

结果保存到 `test-output/plugin-canvas-parity/`。`native.png` 和 `sdk.png` 对应显示器当前比例，指定比例的图片带比例后缀。绘图能力表中未被此场景覆盖的组合和 Apple 原生 surface 仍待各平台实测。

### SVG 图集的释放边界

Windows 隐藏窗口测试使用 GPUI 平台接口访问真实 DirectX 图集。两个不同的 SVG 键各占一个纹理槽。显式删除其中一个键后，第三个键复用了释放的槽，另一个键仍然命中缓存。再次请求已删除的键时会重新构建。[测试代码](../../tests/directx_svg_atlas.rs)

```powershell
cargo test --locked --offline -p nanobug --test directx_svg_atlas -- --ignored --test-threads=1
cargo test --locked --offline -p nanobug --bin nanobug canvas_svg_atlas_releases_retired_keys_in_real_workspace -- --ignored --test-threads=1
```

Windows 和 Linux 的编辑器在创建原生窗口时通过 `PlatformWindow::sprite_atlas` 记录该窗口图集的弱引用。插件的 SVG 仍交给 `Window::paint_svg` 绘制。宿主跟踪插件自己的 SVG 键，并在后续帧确认旧画面已替换后移除无引用项。Windows 工作区实测 100%、125%、150%、200% 四轮开关，同画面共享的键保留到最后一个绘制引用移除。Linux 在 WSLg 的 Wayland 和 X11 上分别用 WGPU 软件适配器验证了真实 SVG 缓存项；替换画面完成后，通过直接调用已安排的回收处理确认缓存键移除。该隐藏窗口用例不测显示回调时间和显存总量。Apple Metal 使用 CPU 上传纹理，当前接口无法确认此前提交的 GPU 绘制已结束，Apple 的 SVG 图集回收与各平台长期显存字节趋势仍待验证。[宿主接入](../../src/ui/native_atlas.rs) · [Windows 工作区回归](../../src/grid/workspace/plugin_manager/cursor_tests.rs) · [GPUI Window 固定版源码](https://docs.rs/crate/gpui-pre/0.3.10/source/src/window.rs) · [DirectX 图集固定版源码](https://docs.rs/crate/gpui-pre-windows/0.3.10/source/src/directx_atlas.rs)

## 2026-11-13 平台补验

同一组已安装的 WASM 在 DirectX、WGPU 离屏及 X11 实际窗口与原生 GPUI 对照。新增组合包含多层裁剪与透明度、描边和嵌套多边形、局部文字样式、下划线、变换 SVG 以及独立解码的 GIF 帧。GPU 回收测试使用实际后续帧。Windows 和 Wayland 软件 WGPU 各完成 1,200 轮，X11 软件 WGPU 完成 240 轮。Windows 后半段的实际进程显存采样保持稳定。Metal 后端现在用 GPU 完成通知保护待复用的纹理，需在 Mac 实机运行同一原生测试。当前数据与方法见[平台与启动补验](2026-11-12-plugin-release-verification.md#2026-11-13-平台与启动补验)。

`Draw::ShapedLine` 走 GPUI 的 `paint_with_underline_handler`。插件可按真实的 UTF-8 字节范围跳过一段下划线、替换样式或指定留空区间。开启标签后，下一帧事件返回原生排版产生的范围、位置、宽度和样式。WASM 要依据这些坐标绘制其他图形时，可提交下一幅画面。旧宿主通过 `Viewport.shaped_line` 的默认值指示使用 `Draw::Text`。[SDK 使用说明](../../plugins/sdk/README.md#canvas-自绘) · [组合对照](../../src/grid/workspace/plugin_manager/canvas/parity.rs)
