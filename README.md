# Doubao Voice Input (豆包语音输入)

Windows 语音输入工具，基于豆包 ASR 实现实时语音识别。

## 功能特性

- 🎤 **实时语音识别** - 基于豆包 ASR 的高精度语音识别
- ⌨️ **点击/按住 Ctrl 触发** - 点击 Ctrl 开始/停止语音输入，按住 Ctrl 录音、松开结束
- 📍 **状态窗口** - 录音时出现的现代风格可拖动状态窗口，左键停止/切换录音，右键退出
- 🔄 **流式识别** - 实时显示识别结果，支持文本修正
- 🖥️ **系统托盘** - 托盘图标菜单控制，右键访问设置和退出
- 📦 **绿色便携** - 单文件可执行，无需安装

## 快速开始

### 下载使用

1. 从 [Releases](https://github.com/EvanDbg/doubao-ime-win/releases) 下载最新版本
2. 解压到任意目录
3. 运行 `doubao-voice-input.exe`
4. 首次运行会自动注册设备

### 使用方法

1. **快捷键** (点击/按住 Ctrl):
   - 点击 `Ctrl` 键开始语音输入
   - 再次点击停止录音，文本自动插入到当前焦点窗口
   - 按住 `Ctrl` 开始录音，松开结束录音

2. **状态窗口**:
   - 🟣 紫色 = 待机状态
   - 🔴 红色 = 正在录音
   - 🟠 橙色 = 处理中
   - **左键点击** = 停止/切换录音
   - **右键点击** = 退出程序（有确认提示）
   - **拖动** = 调整位置

3. **系统托盘**:
   - 右键托盘图标打开菜单
   - 菜单项：开始/停止语音输入、设置、退出

## 配置文件

配置文件 `config.toml` 与程序同目录：

```toml
[general]
auto_start = false
language = "zh-CN"

[hotkey]
mode = "tap_hold"
combo_key = "Ctrl+Shift+V"
double_tap_key = "Ctrl"
double_tap_interval = 300  # 毫秒
tap_hold_key = "Ctrl"    # 点击切换；按住启动、松开结束
tap_hold_threshold = 300 # 按住判定阈值（毫秒）

[floating_button]
enabled = true  # 开始录音时显示状态窗口
position_x = 100
position_y = 100

[asr]
vad_enabled = true
```

## 流式输出诊断

如果识别结果总是有“卡顿感”，可以运行 ASR 流式探测示例，测量麦克风采集、WebSocket 响应和 interim/final 结果间隔：

```powershell
# 默认录音 8 秒，停止录音后最多等待 5 秒收尾
cargo run --example asr_stream_probe -- --duration 8

# 如需更长语音样本
cargo run --example asr_stream_probe -- --duration 15 --drain-timeout 8
```

探测结果会输出：
- `Interim 流式响应`：大于 0 表示当前会话收到流式增量结果。
- `首个文本延迟`：从开始录音到首次收到非空识别文本的耗时。
- `最大响应间隔`：响应之间的最大间隔，可用于判断“卡顿感”是否来自服务端响应不连续。
- 结论行：直接提示本次会话是流式、疑似非流式，还是未收到文本结果。

运行主程序时也可以开启更详细日志查看发送/接收节奏：

```powershell
$env:RUST_LOG = "doubao_voice_input=debug"
cargo run
```

## 从源码构建

### 环境要求

- Rust 1.70+ (stable)
- Windows 10/11 x64
- Visual Studio Build Tools 2022
- CMake
- Protobuf Compiler (protoc)

### 构建步骤

```powershell
# 克隆项目
git clone https://github.com/EvanDbg/doubao-ime-win.git
cd doubao-ime-win

# 构建 Release 版本
cargo build --release

# 可执行文件位置
# target/release/doubao-voice-input.exe
```

### GitHub Actions

项目已配置 GitHub Actions 自动构建：
- 推送到 `main` 分支时自动构建
- 创建 `v*` 标签时自动发布 Release

## 技术架构

| 模块 | 技术 |
|------|------|
| 语言 | Rust |
| 语音识别 | 豆包 ASR (doubaoime-asr 协议) |
| 音频采集 | cpal |
| 音频编码 | Opus |
| 热键监听 | rdev (双击检测) |
| 系统托盘 | tray-icon |
| 悬浮按钮 | Win32 API (Layered Window) |
| 文本输入 | Windows SendInput API |

## 免责声明

> ⚠️ **注意**
> 
> 本项目基于豆包输入法客户端协议分析实现，非官方 API。
> - 仅供学习研究使用
> - 协议可能随时变更导致功能失效
> - 请遵守相关法律法规

## 许可证

MIT License

## 致谢

- [doubaoime-asr](https://github.com/starccy/doubaoime-asr) - 豆包 ASR 协议参考实现
