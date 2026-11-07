# Nanobug V2

[English](README.md)

**项目状态：正在开发中。** 功能和操作方式仍可能变化。

Nanobug V2 是一款使用 Rust 和 GPUI 构建的原生桌面代码编辑器。它采用字符网格界面，内置文件管理器，支持 Vim 风格的编辑命令、平滑光标和滚动动效，以及原生输入法。

## 从源码运行

目前已在 Windows 上验证。安装 Rust 工具链后，在仓库根目录运行：

```powershell
cargo run --locked
```

在 `--` 后传入文件或目录路径，可在启动时打开它。快捷键和开发说明见[使用指南](docs/使用指南.md)。
