# 插件凭据存储：官方接口事实与 Q90 已确认设计

日期：2026-11-11。范围：密码、访问令牌等小型秘密数据的保存边界。用户已对 Q90 回答“是”，采纳下述专用异步接口与存储边界；该方案不是已有实现，也不代表已经通过安全或启动性能验收。本文不选择实现库，不访问真实凭据。

## 官方接口能保证什么

| 对象 | 官方事实与边界 |
| --- | --- |
| VS Code API | `ExtensionContext.secrets` 独立于当前工作区；`SecretStorage` 提供异步 `get/store/delete` 和变更事件，约定加密保存且不跨机器同步。找不到键时 `get` 返回 `undefined`。因此，“独立于工作区”应理解为同一插件切换项目后仍可使用自己的存储，不等于每个项目自动得到独立凭据库。[API 文档](https://code.visualstudio.com/api/references/vscode-api#SecretStorage)、[ExtensionContext](https://code.visualstudio.com/api/references/vscode-api#ExtensionContext) |
| VS Code 桌面实现 | 官方指南说明，桌面端使用 Electron `safeStorage` 加密后写入文件；秘密数据存于客户端，即使插件在远程运行。不能据此说每个令牌都直接保存为 Windows Credential Manager 条目。`workspaceState/globalState` 是普通明文状态，不适合保存秘密。[官方远程插件指南](https://code.visualstudio.com/api/advanced-topics/remote-extensions#persisting-secrets) |
| Windows Credential Manager | `CredWriteW` 写入用户凭据集，并关联当前令牌的登录会话；没有对应凭据集时可以失败，网络登录会话没有该凭据集。这里的主体是 Windows 用户和登录会话，不是编辑器内的插件标识。[CredWriteW](https://learn.microsoft.com/en-us/windows/win32/api/wincred/nf-wincred-credwritew) |
| Windows DPAPI | 通常由相同登录凭据、同一电脑解密，漫游配置文件等存在例外；设置 `CRYPTPROTECT_LOCAL_MACHINE` 会扩大到本机任意用户可解密。Electron 也明确说明其 Windows DPAPI 方案不防同一用户空间的其他应用，因此不能把插件键名前缀当成操作系统隔离。[CryptProtectData](https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata)、[Electron 平台边界](https://www.electronjs.org/docs/latest/api/safe-storage#platform-specific-key-providers) |
| macOS Keychain | Keychain Services 将小型秘密保存在加密数据库，并提供应用访问控制；秘密项的读取可能要求用户输入密码。该应用级保护不能直接解释为宿主内部每个插件都有独立系统身份。[Keychain Services](https://developer.apple.com/documentation/security/keychain-services)、[秘密数据与密码提示](https://developer.apple.com/documentation/security/ksecvaluedata) |
| Linux Secret Service | 条目可能锁定，也可能在调用间重新锁定；解锁可要求服务显示提示，用户取消后操作取消。协议不强制特定访问控制，可允许其他客户端访问已解锁条目。查找属性不属于 secret，可能明文保存，不能放密码或令牌。[Secret Service 第 5、8—10 节](https://specifications.freedesktop.org/secret-service/latest-single/) |

Linux 不保证有可用安全后端。Electron 官方明确说明某些 Linux 环境没有秘密存储，其同步接口的 `basic_text` 后端使用固定明文密码，不能作为可靠的秘密保护。这里仅以它说明风险，未提议 Nanobug 使用 Electron。`safeStorage` 文档还指出，同步调用可能为了 macOS/Linux 的用户输入而阻塞当前线程。[Electron safeStorage](https://www.electronjs.org/docs/latest/api/safe-storage#platform-specific-key-providers)

Secret Service 上述地址当前标为 0.2 DRAFT；引用协议行为不表示所有 Linux 服务都已实现这一草案版本。[规范状态](https://specifications.freedesktop.org/secret-service/latest-single/)

## Nanobug 已确认设计边界

以下接口与存储边界由 Q90 确认，具体后端、接口契约和错误交互仍待实现与验证。后续 Q91 已确认卸载默认保留凭据，清理时由用户明确选择是否删除，不能从普通设置的清理规则推导为自动删除：

1. **宿主统一提供异步 `get/store/delete`。** 宿主根据已验证的调用方身份及当前 profile 分配存储空间，插件只能提交自己的逻辑键，不能自己声称是别的插件或切到日常 profile。这个访问控制由 Nanobug 实现；不能宣传成操作系统天然隔离了插件。
2. **只在本机专用凭据存储中持久保存秘密。** 普通设置、项目 `.nb`、草稿、设置历史和导出内容只保留必要引用，不保存密码或令牌正文；宿主自己的日志不记录秘密参数。Q91 已确认普通卸载保留凭据，现有 Clear local data 确认中默认不选“同时删除已保存的登录信息”；用户明确选择后才清理当前配置环境、已列明插件的专用记录。只有凭据残留时也保留原有清理入口，失败明确报告。本机删除不代表服务端撤销授权，详见[主决策文档 Q91](2026-11-08-plugin-architecture.md)。
3. **明确区分“没有保存过”与“暂时取不到”。** 后端缺失、锁定、用户取消、权限拒绝及读写失败应有明确结果；保存失败不能显示成功，也不能自动退回明文文件或固定密钥的弱保护。具体错误名称与重试入口尚未设计。
4. **凭据访问不进入核心启动等待链。** 编辑器先达到可编辑状态；只有实际需要登录凭据的插件功能才等待异步结果。需要系统解锁时提示用户，由该功能等待或取消；后台适配不能在 UI 线程执行阻塞的系统调用。安装时已确认插件权限，不等于可以跳过系统自身的解锁或授权。

这套方案的目标是避免普通配置泄漏，并控制经宿主 SDK 的访问。依据上面的 Windows 与 Secret Service 边界，不能承诺它能阻止已获得本机执行权限的进程绕过 SDK，也不能承诺拿到原始令牌的插件永远不会自行记录或外传它。
