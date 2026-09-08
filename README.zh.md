<div align=center>
  <img width=200 src="doc\logo.png" alt="logo"/>
  <h1 align="center">CCZU-VPN-PROTO</h1>
</div>

<div align=center>
  <img src="https://img.shields.io/badge/Rust-2024-brown" alt="Rust">
  <img src="https://img.shields.io/github/languages/code-size/CCZU-OSSA/cczu-vpn-proto?color=green" alt="size">
</div>

CCZU WebVPN 隧道客户端的 Rust 开源重实现。

这个项目当前同时提供两条使用路径：

- 一条面向直接使用的 Windows / Linux CLI 客户端路径
- 一条面向嵌入式接入的 Rust 库 / UniFFI bindings 路径

## 特性

- 基于 `tun-rs` 的跨平台 TUN 设备接入
- 面向直接使用的 Windows Wintun / Linux TUN CLI 路径
- 根据 WebVPN 规则数据自动安装 split-tunnel 路由
- 通过 UniFFI 导出 Kotlin、Swift、Python、Ruby 绑定
- 基于 `tracing` 的 CLI 与库级诊断日志

## CLI Usage Guide

CLI 入口位于 [src/main.rs](./src/main.rs)，当前可以直接在 Windows 与 Linux 使用。
Windows 会自动配置 Wintun、DNS 和 split-tunnel 路由；Linux 使用 `ip` 配置 split-tunnel 路由，并使用 `systemd-resolved` 的 `resolvectl` 配置 VPN DNS。

### 面向对象

- 想直接使用 Windows 或 Linux CLI 客户端的用户
- 需要对照真实服务验证协议行为的开发者
- 想在本机快速打通 native tunnel 链路的集成方

### 环境要求

- 已安装 Rust toolchain
- Windows 下需要管理员权限
- Linux 下需要以 root 身份运行，并安装 `iproute2`、`systemd-resolved`、`libsecret` 和 `util-linux`（提供 `ip`、`resolvectl`、`secret-tool`、`runuser`）。程序会调用外部的 `ip` 和 `resolvectl` 配置路由与 DNS；仅给程序文件授予 `CAP_NET_ADMIN` 并不足以保证这些子命令获得所需权限。如果不以 root 运行，还需另行配置相应的 capabilities 与 polkit 授权。
- 能访问 `zmvpn.cczu.edu.cn`

### 运行

PowerShell：

```powershell
$env:RUST_LOG = "info"
cargo run --release --bin cczu-vpn-proto
```

Linux：

```sh
cargo build --release --locked
sudo ./target/release/cczu-vpn-proto
```

首次运行时，程序会提示输入统一身份认证账号和密码；登录成功后输入 `Y`，凭据会加密保存到当前桌面用户的系统密钥环。以后仍执行同一条 `sudo` 命令，程序会自动复用凭据，不再询问账号密码。这里保存的是 VPN 登录凭据，不是 `sudo` 密码；`sudo` 是否再次要求验证由系统的 sudo 缓存策略决定。

如需临时忽略已保存凭据，或删除凭据：

```sh
sudo ./target/release/cczu-vpn-proto --no-keyring
sudo ./target/release/cczu-vpn-proto --forget-login
```

凭据不会以明文配置文件形式写入项目目录。程序以 `sudo` 运行时，会根据 `SUDO_USER` 访问发起命令的桌面用户密钥环，而不是 root 密钥环。按 `Ctrl+C` 正常退出，以便自动清理路由和 DNS 配置。

#### 与 Clash Verge TUN 模式共存

Linux 客户端会安装一条优先级为 `8000` 的策略规则，让 `main` 表中的非默认路由（包括校园网段和 VPN DNS）优先于 Clash Verge / Mihomo 通常安装的 `9000`—`9010` 规则。程序只增删这一条具有完整匹配参数的规则，不会停止 Clash 或修改 Clash 创建的规则，因此 Clash Verge 的 TUN 模式可以保持开启。

如果还要同时开启 Clash Verge 的 **系统代理**，请在当前订阅关联的“规则增强”配置中置顶以下规则（应放在 `prepend`，不要放在 `append`）：

```yaml
prepend:
  - DOMAIN,zmvpn.cczu.edu.cn,DIRECT
  - IP-CIDR,211.65.64.100/32,DIRECT,no-resolve
  - DOMAIN-SUFFIX,cczu.edu.cn,DIRECT
  - IP-CIDR,172.16.0.0/12,DIRECT,no-resolve
```

这会明确保护 VPN 入口 `211.65.64.100`，覆盖截图中的 `172.22.226.31`，并让 `cczu.edu.cn` 域名通过 Mihomo 的 `DIRECT` 出站。客户端会为服务端下发的校园网地址及 VPN DNS 自动安装主路由和优先级 `8000` 的策略规则，所以这些直连流量随后会进入 CCZU VPN。规则增强会随订阅更新保留，比直接修改订阅生成的配置稳定。

如果以后遇到不属于 `172.16.0.0/12` 或 `cczu.edu.cn` 的学校地址，需要把对应 CIDR 或域名继续加入 `prepend`。服务端下发的公共 IPv4 地址通常已由订阅中的中国大陆规则判定为 `DIRECT`，但显式加入规则最可靠。

DNS 仅将 `cczu.edu.cn` 域及其子域交给 VPN DNS（`~cczu.edu.cn`），不再声明全局 `~.`，所以不会与 Clash 的全局 DNS 路由竞争。客户端还会为 VPN DNS 本身添加 `/32` 路由。若校内系统使用不属于 `cczu.edu.cn` 的私有域名，需要再为该域名增加对应的 route-only domain。

如果你的 Clash 配置使用了优先级小于或等于 `8000` 的自定义 `ip rule`，仍需调整其中一方的规则优先级。可以用 `ip rule show` 检查，数字越小优先级越高。

若客户端异常终止，可用以下命令检查是否遗留接口、路由、策略规则或 DNS 配置：

```sh
ip -brief address
ip route show table main
ip rule show
resolvectl status
```

若修改代码后重新构建，必须先按 `Ctrl+C` 退出旧客户端，再启动新二进制；已经运行的进程不会自动更新。

### CLI 会做什么

1. 检查 WebVPN 当前是否看起来可用。
2. 提示输入用户名和密码。
3. 启动隧道会话。
4. 创建一个名为 `CCZU-VPN-PROTO` 的 `tun-rs` 虚拟网卡。
5. 配置平台网络设置：
   - Windows：需要时在当前工作目录落地 `wintun.dll`，并设置 VPN DNS 和 split-tunnel 路由
   - Linux：通过 `ip route` 安装校内 IPv4 split-tunnel 路由，并通过 `resolvectl` 将 VPN DNS 关联到 TUN 接口
6. 在 TUN 设备与自定义 VPN 协议之间做双向包转发。

### 说明

- CLI 默认验证 TLS 服务端证书；库使用方只有显式设置 `no_verification = true` 才会跳过验证。
- 正常退出时会删除本次会话安装的 split-tunnel 路由；Linux 还会通过 `resolvectl revert` 清理接口 DNS 配置。
- Linux 只为 VPN 接口设置 `~cczu.edu.cn` DNS 路由，避免与 Clash Verge 等使用 `~.` 的链路竞争。
- 包收发和路由安装日志都受 `RUST_LOG` 控制。
- 如果你是把它嵌入到别的应用里，通常更适合直接使用 Rust API 或 UniFFI bindings，而不是外部调用 CLI。

## Library Usage

### Rust API

先添加依赖：

```sh
cargo add --git https://github.com/CCZU-OSSA/cczu-vpn-proto.git
```

最小异步示例：

```rust
use anyhow::Result;
use cczuvpnproto::{
    diag,
    types::StartOptions,
    vpn::service,
};

#[tokio::main]
async fn main() -> Result<()> {
    diag::init_tracing("info")?;

    service::start_service_with_options(
        "user",
        "password",
        StartOptions::default(),
    )
    .await?;

    let server = service::proxy_server().expect("proxy server should exist after login");
    println!("{server:?}");
    Ok(())
}
```

### UniFFI bindings

导出的绑定入口在 [src/bindings.rs](./src/bindings.rs)。
当前支持生成的绑定语言：

- Kotlin
- Swift
- Python
- Ruby

发布产物包含：

- 原生库文件（按目标平台提供 `.dll`、`.so`、`.dylib`、`.a`、`.lib`）
- 每种支持语言对应的 UniFFI 绑定压缩包

## Develop Guide

### 项目结构

- [src/main.rs](./src/main.rs)：CLI 测试入口
- [src/bindings.rs](./src/bindings.rs)：UniFFI 导出层
- [src/types.rs](./src/types.rs)：共享类型和启动选项
- [src/vpn/service.rs](./src/vpn/service.rs)：会话 actor 和运行时状态
- [src/vpn/protocol](./src/vpn/protocol)：自定义协议的读写实现
- [src/diag.rs](./src/diag.rs)：`tracing` 初始化辅助

### 常用命令

格式化：

```sh
cargo fmt
```

检查：

```sh
cargo check --locked
```

构建 release：

```sh
cargo build --release --locked
```

只编译测试目标，不执行：

```sh
cargo test --locked --no-run
```

### 本地生成 UniFFI bindings

先启用 bindgen feature：

```sh
cargo build --release --locked --features bindgen
cargo build --locked --features bindgen --bin uniffi-bindgen
```

然后基于宿主机生成的动态库，按语言分别生成 bindings：

Windows 宿主：

```powershell
.\target\debug\uniffi-bindgen.exe generate -n -l kotlin -o .\dist\bindings\kotlin .\target\release\cczuvpnproto.dll
.\target\debug\uniffi-bindgen.exe generate -n -l swift -o .\dist\bindings\swift .\target\release\cczuvpnproto.dll
.\target\debug\uniffi-bindgen.exe generate -n -l python -o .\dist\bindings\python .\target\release\cczuvpnproto.dll
.\target\debug\uniffi-bindgen.exe generate -n -l ruby -o .\dist\bindings\ruby .\target\release\cczuvpnproto.dll
```

Linux 宿主：

```sh
target/debug/uniffi-bindgen generate -n -l kotlin -o ./dist/bindings/kotlin ./target/release/libcczuvpnproto.so
```

macOS 宿主：

```sh
target/debug/uniffi-bindgen generate -n -l swift -o ./dist/bindings/swift ./target/release/libcczuvpnproto.dylib
```

### 发布工作流

- [nightly.yml](./.github/workflows/nightly.yml)：定时或手动触发的 `pre-release` 预发布工作流
- [release.yml](./.github/workflows/release.yml)：推送 `v*` tag 时触发的正式发布工作流

两条 workflow 都会发布：

- matrix 中各目标平台的原生库
- Kotlin、Swift、Python、Ruby 的 UniFFI 绑定压缩包
