# ssh2socks

基于 `~/.ssh/config` 的 SSH 隧道桌面小工具：一键开 SOCKS5 代理（`ssh -D`）、本地端口转发（`ssh -L`）、远程端口转发（`ssh -R`）。
Rust + Tauri v2 实现，安装包/可执行文件只有几 MB，支持 Windows、macOS、Linux。

## 功能

- 从 `~/.ssh/config` 读取主机（支持 `Include`，可搜索）
- 三种隧道类型：
  - **SOCKS5 代理**：本机开一个 SOCKS5 代理，流量经服务器出去
  - **本地转发**：把服务器上（或服务器所在局域网里）的端口映射到本机 `127.0.0.1:端口`
  - **远程转发**：把本机端口发布到服务器 `0.0.0.0:端口`，对外提供访问
  - 两种转发都有「打开」按钮，可以直接在浏览器里打开对应的 http 地址
- 首次连接新服务器时自动信任其主机密钥（`StrictHostKeyChecking=accept-new`），不再卡在 yes/no 确认；已记录的密钥如果变了仍会拒绝，并提示处理方法
- 一键启动 / 停止、全部启动 / 全部停止
- 断线自动重连（1s → 30s 指数退避），`ServerAliveInterval` 保活
- 连通性探测，每 30 秒一次：
  - SOCKS：通过代理访问探测地址。`http://` 地址会完整发起一次 HTTP 请求；`https://` 地址只要 SOCKS CONNECT 成功就算连通（不做 TLS 握手）
  - 本地转发：检查转发出去的连接会不会被立即断开（服务器连不上目标时会断开）
  - 远程转发：先确认本机服务在运行，再从本机访问 `服务器:端口`
- 报错及时可见：日志每行都带时间；列表里直接显示最近一次错误；连接超过 10 秒还没建立就在日志里提示，超过 45 秒则放弃这次尝试并自动重连
- 系统托盘常驻，关闭窗口不会断开隧道
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

## 远程转发注意事项

sshd 默认只允许远程转发监听服务器的 `127.0.0.1`。要让外网访问到，需要在服务器的 `/etc/ssh/sshd_config` 里设置：

```
GatewayPorts clientspecified   # 或 yes
```

改完重启 sshd，并确认服务器防火墙或云安全组已放行对应端口。

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
