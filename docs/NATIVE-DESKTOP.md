# 原生轻量桌面端迁移方案

2026-11-17。用户希望最终客户端不再使用 Tauri，减少桌面资源开销。目标仍为 Windows / macOS / Linux，以及 Windows x64 单文件；TS3 完整非官方线上服务功能范围继续由 TS3-PARITY.md 管理。

## 推荐方向

使用 Rust + egui/eframe 的桌面原生窗口集成，先评估 Glow 渲染后端。界面自行绘制，不是 Windows/macOS 的原生控件外观，也不使用浏览器或 WebView2。最终是否采用 Glow，必须通过低端显卡、远程桌面、虚拟机和三端实际启动验证。

选择理由：现有 Session、协议、身份存储、CPAL、Opus 和声音处理均已是独立 Rust crate。原生界面可以直接调用 Session，不需要重写 TS3 协议或增加 C++ / Rust 桥接。界面应优先采用适合语音客户端的紧凑频道树、聊天区、用户区和设置窗口，最终外观按用户偏好调整。

替代方案的取舍：

| 方案 | 项目适配 | 主要取舍 |
| --- | --- | --- |
| egui/eframe | 直接复用 Rust；推荐先做验证 | 自绘控件；IME、可访问性、显卡/远程桌面必须实测 |
| Slint | 可连接 Rust；可作为验证失败时的备选 | 需要新的声明式界面层；具体后端、授权和发布要求需在采用时复核 |
| Qt | 桌面功能成熟，适合传统客户端 | 需要额外集成与部署工作；单文件和发布方式需专项评估 |

这里的“推荐”不是实测性能结论。当前 Windows 0.3.2 EXE 已约 17.48 MiB，且未内置 WebView2 Runtime；去掉 WebView 不意味着 EXE 一定明显更小。收益主要希望体现在进程结构、内存与界面调度，必须用同一设备上的对照数据证明。

## 保留与迁移范围

直接复用：

- crates/core 的快照、事件、连接和音频设置模型。
- crates/teamspeak 的 Session、命令确认、连接与身份逻辑。
- crates/audio 的设备、采播、Opus、DSP、VAD/PTT、恢复和诊断。

重新实现：

- Tauri 窗口、React/CSS、IPC 调用和浏览器本地存储。
- 原生菜单/窗口、剪贴板、文件选择、全局热键、托盘和系统通知适配。
- 收藏、最近连接等本地持久化；音频设置与已有身份的兼容迁移。

音频掉线/语音激活故障在 Rust 音频和会话链路中。替换界面框架不会自动解决这些故障，音频稳定性验收独立进行。

## 实施路径与替换条件

1. **原型与性能门槛**：新增独立 apps/native 入口，连接现有 Session，实现真实连接、频道树、收发聊天、音频状态和完整现有音频设置；验证中文显示/输入法、DPI、窗口缩放、剪贴板和 Windows 单文件。此阶段保留现有客户端，方便功能和资源对照。
2. **现有功能迁移**：按 0.3.2 的真实入口逐项迁移频道切换、私聊/未读、收藏、个人音量、用户信息、自身动作、频道编辑及带确认的管理操作。不能把仅能打开窗口的原型当作功能完整的新客户端。
3. **切换正式入口**：旧客户端已有功能验收通过，并确认资源收益后，更新默认构建/打包/CI/文档，再移除旧界面依赖。沿用既有 TS3 对齐清单完成其余功能，不重置或缩减范围。

## 调度和状态约束

- GUI 在主线程；Tokio 和音频工作线程独立运行，GUI 不等待网络、磁盘或 DSP。
- 继续使用有界命令、watch 快照、broadcast 事件与 request ID 确认，避免界面直接修改协议状态。
- 静止界面按事件唤醒，不进行无意义的持续 60 FPS 重绘。
- 电平可见时限频更新；隐藏/最小化时暂停可视电平重绘，保持后台通话和按键响应。
- 频道和历史消息采用可见区域布局；限制内存消息与诊断历史。
- 合并重复音频快照唤醒；队列积压不能阻塞采集或造成旧音频重发。
- 原生与旧版配置兼容；密码不写进普通设置或收藏，系统凭据身份不改为明文文件。

## 对照验收

同一 Windows 设备、相同服务器/频道、设备和 DSP 设置，分别测量旧版和原生版：

| 场景 | 记录 |
| --- | --- |
| 启动与空闲 | 冷启动时间、所有相关进程、CPU、私有内存/工作集 |
| 连接后静默 | CPU、内存、唤醒/重绘、频道树操作 |
| 双向通话 | CPU、内存、延迟、编码/发送/接收计数、音频恢复 |
| 窗口最小化/托盘 | 后台 CPU、内存、热键和发言恢复 |
| 长时运行与丢包 | 资源趋势、故障恢复、重连、无永久静音 |
| 发布 | EXE 字节数、系统依赖、中文 IME、DPI、显卡与三端启动 |

不以开发机交叉编译成功替代 Windows 运行结果，不预先承诺未测量的内存数字或缩减比例。

## 官方技术参考

- [egui 仓库：原生平台、限制与空闲重绘](https://github.com/emilk/egui)
- [eframe 框架与功能选项](https://docs.rs/eframe/latest/eframe/)
- [eframe 渲染后端](https://docs.rs/eframe/latest/eframe/enum.Renderer.html)
- [egui 重绘控制](https://docs.rs/egui/latest/egui/struct.Context.html#method.request_repaint_after)

## 0.4.0 实现与验证（2026-11-18）

用户确认采用接近 TS3 的紧凑实用界面。已新增独立 `apps/native`，使用 egui/eframe 0.37 + Glow、系统字体回退和 AccessKit。原生目标依赖树中没有 Tauri / Wry / WebKit / WebView2 / wgpu。旧界面继续保留，尚未将全量 TS3 功能对齐标记完成。

本批接入：连接、取消/断开、频道前序链排序/树、搜索与加入、用户详情/个人音量、频道/服务器/私聊标签与未读、消息确认/错误状态、自身昵称/离开/订阅/密钥、Poke/移动/踢出确认入口、频道编辑入口（未加载描述时禁止覆盖）、原生收藏增删改、完整既有音频配置/本机测试/全局 F8–F12/界面 PTT/恢复/诊断、深浅主题。输入/网络/DSP 在独立线程，不由 UI 绘制帧驱动。

已确认的迁移边界：

- 沿用同一系统凭据中的默认身份、同一 `audio.json`；每次启动仍强制麦克风静音。原生收藏及最近连接保存在 `native.json`，不含密码。
- 不自动读取 WebView 的 localStorage 收藏；原生版可重新添加，旧数据未删除。批量迁移/导入导出仍待做。
- macOS 本地构建包首次读取原系统身份时，在系统钥匙串调用中阻塞（线程栈已确认）。已将身份准备移出会话阻塞分支，支持立即取消并加入 10 秒超时；未返回的系统调用不会触发后台迟到连接，也不会在重试时不断新增凭据读取任务。没有删除身份、修改 ACL 或回退明文。
- 连接窗口提供显式临时测试身份，不继承原身份权限、不覆盖系统身份；安全等级升级不会保存到原身份。测试仅用该入口。
- 全局热键在 Linux 仍限 X11；Wayland Portal、托盘、通知及其余 TS3 功能仍按原验收表跟踪。
- 普通文本已接入，富文本/BBCode、磁盘聊天历史等尚未完成；窗口目前为桌面内容窗口，设置等为其内部浮动面板。

验证：

- 17 项核心/音频/协议/原生测试通过。新增测试覆盖：身份读取期间立即取消并防止迟到连接、频道顺序/孤儿/循环、无变化的心跳不唤醒空闲窗口。
- 原生与协议所有目标严格 Clippy 通过；Windows x64 release 交叉构建，PE GUI/架构/外部 DLL 检查通过。Windows 实机尚未运行，CI 配置已补充但未在线执行。
- macOS 原生窗口成功运行，用临时身份连接授权服务器并显示目标频道。已检查中文显示与搜索、用户详情、私聊标签、音频设置保存和断开；麦克风保持静音，没有给其他用户发消息或执行管理操作。
- 中文字符输入不等同于三端 IME 候选窗验收；Windows/macOS/Linux 输入法组合、低端显卡、远程桌面及双向人声通话仍需验证。
- macOS 单次断开后观测：原生进程 RSS 105,248 KiB（约 102.8 MiB），CPU 快照 0.6%。该观测不是持续平均值，也没有同机同场景旧版对照，不宣称具体节省比例，更不能代表 Windows。

交付：`releases/windows/CongSpeak-Native-0.4.0-windows-x64.exe`，17,727,488 字节（16.91 MiB），SHA-256 `c7d61cbdc91ace85504b047acc5a146b01240095a2ea6d355feafe4a80498df8`，不依赖 WebView2 或额外 VC/Opus DLL；仍依赖 Windows 系统库、字体和可用的 OpenGL 驱动。原生 macOS 本地 app 包未签名/公证。

构建：

```sh
cargo run --release -p congspeak-native
# Windows 本机，或已配置 cargo-xwin 的跨平台构建环境
node scripts/build-native.mjs
# macOS release 构建后创建本地 app 包
cargo build --release -p congspeak-native
python3 scripts/bundle-native-macos.py
```


## 0.4.1 空闲内存（2026-11-18）

默认窗口从 1080×720 调为 920×600，仍可调整大小并使用高分辨率渲染。采集 DSP 和发送编码器按需分配，在重置或断开时释放。RNNoise 仅在选用该模式时创建。仅收听时跳过回声参考处理，播放复用重采样缓冲。

同一台 Apple Silicon Mac，依次启动两版 release 应用，使用各版本默认窗口。未连接、麦克风静音、关闭设置窗口。

| 指标 | 0.4.0 | 0.4.1 |
| --- | ---: | ---: |
| `vmmap` Physical footprint | 102.3 MiB | 87.5 MiB |
| `ps` RSS（三次采样一致） | 114.2 MiB | 103.3 MiB |
| `vmmap` IOSurface 驻留 | 59.8 MiB | 42.5 MiB |

分开运行无界面的对照程序，增加八个空闲音频工作线程时，RSS 增量从 11,152 KiB 降到 544 KiB。该对照包含进程共享初始化，不能折算为单个工作线程的固定开销。

Windows x64 单文件已完成交叉构建和依赖检查。Windows 的进程内存与通话期间的内存仍需真机测量。


## 轻量框架对照（2026-11-18）

在同一台 Apple Silicon Mac 依次运行 egui/Glow 和 FLTK 对照样机。两者均使用 920×600 窗口、100 条中文测试频道，并初始化相同的 CongSpeak 会话和音频任务。测量时未连接、未打开音频设备。样机是用于比较界面开销的简化程序。

| 指标 | egui 0.37 / Glow | fltk-rs 1.5.23 |
| --- | ---: | ---: |
| `vmmap` Physical footprint | 86.5 MiB | 105.0 MiB |
| `ps` RSS（三次抽样的中位数） | 118.6 MiB | 118.7 MiB |
| `vmmap` IOSurface 驻留 | 42.5 MiB | 17.0 MiB |

FLTK 降低了图形表面的驻留量，两版 RSS 接近，FLTK 的总 physical footprint 更高。采用 FLTK 之前需要 Windows 同场景实测。若 Windows 的内存优先，可用 Win32 系统树控件和文本控件做独立对照。若三端共用 UI 优先，可继续测 Slint 的软件渲染。

样机源码和原始测量保存在本地 `target/gui-memory-comparison/`。框架信息见 [FLTK 介绍](https://www.fltk.org/doc-1.4/intro.html)、[Win32 常用控件](https://learn.microsoft.com/en-us/windows/win32/controls/common-control-window-classes) 和 [Slint 渲染选项](https://docs.slint.dev/latest/docs/slint/guide/backends-and-renderers/backend_winit/)。
