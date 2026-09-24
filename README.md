# ssh2socks

把 `~/.ssh/config` 里的主机一键变成本地 SOCKS5 代理（`ssh -D`）的桌面小工具。
Rust + Tauri v2 实现，安装包/可执行文件只有几 MB，支持 Windows、macOS、Linux。

## 功能

- 从 `~/.ssh/config` 读取主机（支持 `Include`，可搜索），为每条隧道指定本地端口
- 一键启动 / 停止、全部启动 / 全部停止
- 断线自动重连（1s → 30s 指数退避），`ServerAliveInterval` 保活
- 连通性探测：通过代理访问探测地址并显示延迟
  - `http://` 地址：完整发起一次 HTTP 请求
  - `https://` 地址：SOCKS CONNECT 成功即视为连通（不做 TLS 握手）
- 实时日志面板；系统托盘常驻，关闭窗口不会断开隧道
- 退出时清理所有 ssh 子进程（Windows 用 Job Object，Linux 用 `PR_SET_PDEATHSIG`）

## 运行要求

- 系统自带 OpenSSH 客户端（`ssh` 在 PATH 中）；Windows 10/11 默认已安装
- 使用**密钥免密登录**（以 `BatchMode=yes` 运行，不会弹出密码输入）
- Windows：需要 WebView2 运行时（Win11 自带；安装包会自动下载）
- Linux：需要 `libwebkit2gtk-4.1`、`libayatana-appindicator3`（deb/rpm 包会自动拉依赖）

## 下载

在 [Actions](../../actions) 的每次构建里下载 artifacts，或在 [Releases](../../releases) 下载正式版本：

| 平台 | 文件 |
| --- | --- |
| Windows | `*_x64-setup.exe`（NSIS 安装包）、`*.msi`、`*_portable.exe`（免安装单文件） |
| macOS | `*_universal.dmg`（Intel + Apple Silicon） |
| Linux | `*.AppImage`、`*.deb`、`*.rpm`、`*_linux_x64`（裸二进制） |

> macOS 包未经公证。首次打开请右键 → 打开，或执行 `xattr -cr /Applications/ssh2socks.app`。

## 配置文件

与旧版 Python 实现兼容，位于：

- Linux：`~/.config/ssh2socks/tunnels.json`
- macOS：`~/Library/Application Support/ssh2socks/tunnels.json`
- Windows：`%APPDATA%\ssh2socks\tunnels.json`

## 本地构建

需要 Rust（stable）、Node.js 20+，以及 [Tauri 的系统依赖](https://v2.tauri.app/start/prerequisites/)。

```bash
npm ci
npx tauri dev      # 开发运行
npx tauri build    # 打包，产物在 src-tauri/target/release/bundle/
```

测试与检查：

```bash
cd src-tauri
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## CI / 发布

`.github/workflows/ci.yml`：

- 每次推送 / PR：fmt + clippy + 测试，然后在 Windows、macOS、Linux 上构建并上传产物
- 推送 `v*` 标签（如 `git tag v0.2.0 && git push origin v0.2.0`）：额外创建 GitHub Release 并附上全部安装包

## 许可证

Apache-2.0
