# CongSpeak

CongSpeak 是面向 Windows、macOS、Linux 的第三方 TeamSpeak 3 兼容客户端（当前版本 0.5.9）。项目独立开发，采用 MIT 许可证。[项目声明](NOTICE.md) · [MIT 许可证](LICENSE) · [功能对齐清单](docs/TS3-PARITY.md)。

已实现：Rust 会话任务、真实 TS3 连接与取消/断开、频道/用户快照与频道管理、频道文件浏览与传输、多身份管理与 TS3 INI 导入导出、UID 联系人与忽略、服务器封禁列表与投诉、系统凭据身份存储接入，以及基于 Rust + egui/eframe 的紧凑原生桌面界面。

实时音频已接入桌面应用：Opus 接收/混音/播放、麦克风采集/处理/编码/发送、标准降噪和 RNNoise、回声消除及自动增益。默认麦克风静音；协议 CLI 仍保持音频禁用。详细验证范围见 [音频实现记录](docs/AUDIO.md)。

使用方法：连接服务器后打开音频设置，选择设备，试听并应用；解除麦克风静音后，按住 F8（可改 F9–F12）或界面发言按钮讲话，也可切换语音激活/持续发送。每次启动强制麦克风静音，设置自动保存到系统应用配置目录的 `audio.json`，不保存服务器密码。

「语音激活」使用本地人声检测。音频设置里可以调「人声检测灵敏度」，并通过麦克风试听观察人声置信度。升级后旧的 dB 阈值由默认灵敏度替代。

完整路线见 [实施计划](docs/PLAN.md)，历史验证记录见 [M0 记录](docs/M0.md) 与 [原生桌面迁移记录](docs/NATIVE-DESKTOP.md)。

## 启动与打包原生桌面端（0.5.9）

首页提供最近连接和服务器收藏。默认采用 440 × 720 的竖向窗口，上方列出频道和用户，下方显示聊天和语音控制。拖动聊天上沿可调整高度。拉宽窗口可切换为左右布局，「视图 → 竖向窗口」可一键恢复。点击频道栏的「详情」可查看选中对象，窗口较窄时详情在浮窗中打开。

```sh
cargo run --release -p congspeak-native
```

连接成功后会自动请求订阅频道。频道行显示人数或订阅状态，点「有人」可筛选已看到成员的频道。成员加入和退出服务器时，「事件」页会记录昵称与频道。「视图 → 加入与退出提示音」可切换本地提示音。

Windows 单文件位于 `releases/windows/CongSpeak-Native-0.5.9-windows-x64.exe`。源码打包使用 `node scripts/build-native.mjs`，macOS app 包脚本为 `python3 scripts/bundle-native-macos.py`。Windows 上的 OpenGL、输入法和实际通话仍需真机验证。

原生版提供频道管理、频道文件浏览与传输、UID 联系人、收藏分组和当前会话聊天搜索。文件窗口可从频道右键菜单或频道信息区打开，目录创建、重命名、删除与传输操作均在文件窗口内联完成。上传与下载使用本地完整路径，上传也支持拖入文件。

顶部「身份」提供本地身份新建、重命名、删除、详情和 TS3 INI 导入导出。连接和收藏可分别选用身份。顶部「管理」提供服务器封禁列表和解除封禁，用户右键菜单与详情提供投诉、发言权、移动、踢出和限时封禁入口。

## TS3 功能对齐

用户已确认按 TS3 桌面客户端对齐。完整跟踪见 [逐项验收表](docs/TS3-PARITY.md)，排除官方账号、云同步和社区服务，不排除文件传输等直连网络能力。

0.3 本批新增：选择频道后加入（支持密码）、频道搜索、服务器/频道/私聊分页与未读、昵称/离开/订阅/Poke、用户信息与个人音量/静音、用户移动/踢出、频道名称/主题/描述编辑、权限密钥兑换、本地收藏增删改和最近连接填写。服务器操作均等待命令确认；有相应权限才会成功。

频道文件的空目录读取已在授权服务器上验证，文件往返及管理操作仍需验收。完整权限编辑、多连接、Whisper、录音、托盘和通知仍在清单中待实现。聊天目前按标签保留当前会话最近 500 条，个人音量在当前会话生效。

## 开发环境

- Rust 1.99.0（由 `rust-toolchain.toml` 固定）。
- 平台原生编译环境（C/C++ 编译器与 CMake，用于 Opus 静态构建）。
- 实时处理使用 Sonora 纯 Rust WebRTC APM 实现和 nnnoiseless；可选 `webrtc` 对照测试使用原生 C++ APM，需要 Meson、Ninja、pkg-config。
- macOS 音频模块的部署基线为 14.2；其他端运行验证仍待完成。

macOS 首次安装构建工具：

```sh
brew install cmake pkgconf meson ninja
```

Linux 还需要 ALSA、D-Bus 及 X11/Wayland/OpenGL 开发依赖；详见 CI 安装步骤。

## 构建本机与跨平台应用包

构建 macOS `.app` 调试包：

```sh
cargo build --release -p congspeak-native
python3 scripts/bundle-native-macos.py
```

构建 Windows x64 单文件便携版（`releases/windows/CongSpeak-Native-0.5.9-windows-x64.exe`）：

```sh
node scripts/build-native.mjs
```

Windows 本机需要 MSVC C++ 构建工具；macOS/Linux 则先安装 LLVM、LLD、cargo-xwin 和 `x86_64-pc-windows-msvc` Rust 目标，并把工具加入 PATH（`brew install llvm lld`）。打包脚本校验 PE 架构、GUI 子系统和外部运行库依赖，并生成 SHA-256 校验和。

## 协议验证工具

在仓库根目录：

```sh
cargo run -p congspeak-probe -- --address localhost:9987 --channel /6 --observe-seconds 10
```

需要密码时添加 `--password-stdin`，通过标准输入传递；不要把密码写进命令行参数、shell 历史或提交到仓库。默认使用系统凭据中的身份，添加 `--ephemeral` 明确使用临时身份。探针只读状态并正常断开，不发送语音或聊天。

`--json` 输出频道/用户快照（包含服务器提供的名字，请自行决定是否分享）；`--observe-seconds 0` 持续观察到 Ctrl+C。

## 音频验证

```sh
# 本地设备枚举；不打开采集/播放流。
cargo run -p congspeak-audio --example audio_probe -- --devices

# RNNoise 合成 PCM 性能验证。
cargo run -p congspeak-audio --example audio_probe --release

# RNNoise 与 WebRTC NS/AEC/AGC 分别处理合成帧，不叠加降噪。
cargo run -p congspeak-audio --example audio_probe --features webrtc --release
```

`nnnoiseless` 是 RNNoise 的纯 Rust 实现，便于先验证算法链路；尚未声称与最新 Xiph RNNoise 模型完全等效。WebRTC 使用 `webrtc-audio-processing ~2.1.0` 的 bundled 模式，不依赖用户另外安装运行时动态库。

这些命令只验证库可构建、输入输出正常及合成帧处理耗时，不能替代人声保真、真实回声、双向讲话和设备时钟同步测试。

## 检查

```sh
cargo fmt --all -- --check
cargo test -p congspeak-core -p congspeak-teamspeak -p congspeak-audio -p congspeak-native
cargo clippy -p congspeak-probe -p congspeak-native --all-targets -- -D warnings
```

CI 配置包含三端核心/音频测试和桌面编译，以及 macOS/Linux 的 WebRTC 构建与合成帧验证。添加工作流不代表已完成远程执行或三端真机验证。

## 上游边界

协议依赖固定于 `ReSpeak/tsclientlib` 的 `ee3bc6f45a7137db7793ba5593a321df400d53e5`，包含声明子模块。适配层在终止错误后直接丢弃连接，避免继续轮询上游已结束的 future。连接建立过程中取消会停止本地任务，但上游握手中已分配的服务器席位仍可能保留到服务器超时（Issue #43）；该问题尚未宣称修复。

首版仅验证 Opus 频道。不会自动重发发送结果不明的消息。

语音激活稳定性修复、验证证据与剩余边界见 [docs/STABILITY.md](docs/STABILITY.md)。
