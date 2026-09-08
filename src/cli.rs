use std::{
    io::{BufRead, Write, stdin, stdout},
    sync::Arc,
};

#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail, ensure};
use cczuni::impls::services::webvpn::WebVPNService;
use cczuvpnproto::{diag, vpn::service};
use clap::Parser;
use rpassword::prompt_password;
use tracing::{debug, error, info, warn};
use tun_rs::{DeviceBuilder, InterruptEvent};

#[derive(Debug, Parser)]
#[command(name = "cczu-vpn-proto")]
#[command(about = "CCZU WebVPN CLI client")]
struct CliArgs {
    #[arg(long)]
    user: Option<String>,
    #[arg(long)]
    password: Option<String>,
    #[arg(long, default_value = "info")]
    log_level: String,
    #[arg(long)]
    yes: bool,
    /// Do not read or save credentials in the Linux desktop keyring.
    #[arg(long)]
    no_keyring: bool,
    /// Delete credentials saved in the Linux desktop keyring and exit.
    #[arg(long)]
    forget_login: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedCredentials {
    user: String,
    password: String,
}

impl std::fmt::Debug for SavedCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SavedCredentials")
            .field("user", &self.user)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct KeyringUser {
    name: String,
    runtime_dir: String,
}

#[cfg(target_os = "linux")]
fn keyring_user() -> Option<KeyringUser> {
    let name = std::env::var("SUDO_USER")
        .ok()
        .filter(|name| !name.is_empty() && name != "root")
        .or_else(|| {
            std::env::var("USER")
                .ok()
                .filter(|name| !name.is_empty() && name != "root")
        })?;
    let uid = std::env::var("SUDO_UID")
        .ok()
        .filter(|uid| uid.bytes().all(|byte| byte.is_ascii_digit()))
        .or_else(|| {
            let output = Command::new("id").args(["-u", &name]).output().ok()?;
            output
                .status
                .success()
                .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        })?;
    Some(KeyringUser {
        name,
        runtime_dir: format!("/run/user/{uid}"),
    })
}

#[cfg(target_os = "linux")]
fn secret_tool_command(keyring_user: &KeyringUser) -> Command {
    let mut command = Command::new("runuser");
    command.args([
        "-u",
        &keyring_user.name,
        "--",
        "env",
        &format!("XDG_RUNTIME_DIR={}", keyring_user.runtime_dir),
        &format!(
            "DBUS_SESSION_BUS_ADDRESS=unix:path={}/bus",
            keyring_user.runtime_dir
        ),
        "secret-tool",
    ]);
    command
}

#[cfg(target_os = "linux")]
fn load_saved_credentials() -> Result<Option<SavedCredentials>> {
    let Some(keyring_user) = keyring_user() else {
        return Ok(None);
    };
    let output = secret_tool_command(&keyring_user)
        .args(["lookup", "service", "cczu-vpn-proto"])
        .output()
        .context("failed to run secret-tool lookup")?;
    if !output.status.success() || output.stdout.is_empty() {
        return Ok(None);
    }
    let secret = String::from_utf8(output.stdout).context("keyring secret is not valid UTF-8")?;
    serde_json::from_str(secret.trim())
        .map(Some)
        .context("saved CCZU credentials are invalid")
}

#[cfg(not(target_os = "linux"))]
fn load_saved_credentials() -> Result<Option<SavedCredentials>> {
    Ok(None)
}

#[cfg(target_os = "linux")]
fn save_credentials(credentials: &SavedCredentials) -> Result<()> {
    let keyring_user = keyring_user().context("could not determine desktop user for keyring")?;
    let mut child = secret_tool_command(&keyring_user)
        .args([
            "store",
            "--label=CCZU VPN login",
            "service",
            "cczu-vpn-proto",
        ])
        .stdin(Stdio::piped())
        .spawn()
        .context("failed to run secret-tool store")?;
    let secret = serde_json::to_vec(credentials).context("failed to encode saved credentials")?;
    child
        .stdin
        .take()
        .context("secret-tool stdin was not available")?
        .write_all(&secret)
        .context("failed to send credentials to secret-tool")?;
    ensure!(
        child
            .wait()
            .context("failed to wait for secret-tool store")?
            .success(),
        "secret-tool failed to save credentials"
    );
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn save_credentials(_credentials: &SavedCredentials) -> Result<()> {
    bail!("system keyring login is currently supported only on Linux")
}

#[cfg(target_os = "linux")]
fn forget_saved_credentials() -> Result<()> {
    let keyring_user = keyring_user().context("could not determine desktop user for keyring")?;
    let status = secret_tool_command(&keyring_user)
        .args(["clear", "service", "cczu-vpn-proto"])
        .status()
        .context("failed to run secret-tool clear")?;
    ensure!(status.success(), "secret-tool failed to clear credentials");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn forget_saved_credentials() -> Result<()> {
    bail!("system keyring login is currently supported only on Linux")
}

fn read_prompt(prompt: &str) -> Result<String> {
    print!("{prompt}");
    stdout().flush().context("failed to flush stdout prompt")?;

    let mut value = String::new();
    stdin()
        .lock()
        .read_line(&mut value)
        .context("failed to read console input")?;

    Ok(value.trim().to_string())
}

fn read_required_prompt(prompt: &str) -> Result<String> {
    loop {
        let value = read_prompt(prompt)?;
        if !value.is_empty() {
            return Ok(value);
        }
        warn!(prompt, "received empty input, asking again");
    }
}

fn read_password_prompt(prompt: &str) -> Result<String> {
    prompt_password(prompt).context("failed to read password input")
}

fn read_required_password_prompt(prompt: &str) -> Result<String> {
    loop {
        let value = read_password_prompt(prompt)?;
        if !value.is_empty() {
            return Ok(value);
        }
        warn!(prompt, "received empty password, asking again");
    }
}

fn resolve_credentials(args: &CliArgs) -> Result<(SavedCredentials, bool)> {
    if !args.no_keyring && args.user.is_none() && args.password.is_none() {
        match load_saved_credentials() {
            Ok(Some(credentials)) => {
                info!(user = %credentials.user, "using credentials from the system keyring");
                return Ok((credentials, true));
            }
            Ok(None) => {}
            Err(err) => {
                warn!(error = %err, "could not read the system keyring; falling back to prompts");
            }
        }
    }

    let user = match &args.user {
        Some(user) if !user.trim().is_empty() => user.trim().to_string(),
        _ => read_required_prompt("用户: ")?,
    };

    let password = match &args.password {
        Some(password) if !password.trim().is_empty() => password.trim().to_string(),
        _ => read_required_password_prompt("密码（输入已隐藏）: ")?,
    };

    Ok((SavedCredentials { user, password }, false))
}

fn confirm_continue(prompt: &str) -> Result<bool> {
    let choice = read_prompt(prompt)?;
    Ok(choice.trim().to_lowercase() != "n")
}

fn confirm_explicitly(prompt: &str) -> Result<bool> {
    Ok(read_prompt(prompt)?.trim().eq_ignore_ascii_case("y"))
}

#[cfg(target_os = "windows")]
fn ensure_wintun_dll() -> std::io::Result<()> {
    let dll_path = std::path::Path::new("wintun.dll");
    if !dll_path.exists() {
        info!(path = %dll_path.display(), "creating wintun dll");
        std::fs::write(dll_path, include_bytes!("../wintun.dll"))?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct WindowsRoute {
    destination: std::net::Ipv4Addr,
    netmask: std::net::Ipv4Addr,
}

fn prefix_to_netmask(prefix: u8) -> Result<std::net::Ipv4Addr> {
    if prefix > 32 {
        bail!("invalid IPv4 prefix length: {prefix}");
    }
    let bits = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - u32::from(prefix))
    };
    Ok(std::net::Ipv4Addr::from(bits))
}

fn normalize_network(
    address: std::net::Ipv4Addr,
    netmask: std::net::Ipv4Addr,
) -> std::net::Ipv4Addr {
    std::net::Ipv4Addr::from(u32::from(address) & u32::from(netmask))
}

fn netmask_to_prefix(netmask: std::net::Ipv4Addr) -> Result<u8> {
    let bits = u32::from(netmask);
    let prefix = bits.count_ones() as u8;
    ensure!(
        bits == u32::from(prefix_to_netmask(prefix)?),
        "non-contiguous IPv4 netmask: {netmask}"
    );
    Ok(prefix)
}

fn parse_split_tunnel_route(route: &str) -> Result<Option<WindowsRoute>> {
    let trimmed = route.trim();
    if trimmed.is_empty() {
        bail!("received an empty split-tunnel route");
    }

    let route_target = if trimmed.contains(';') {
        let mut parts = trimmed.split(';');
        let _protocol = parts
            .next()
            .with_context(|| format!("invalid split-tunnel rule format: {trimmed}"))?;
        let target = parts
            .next()
            .with_context(|| format!("invalid split-tunnel rule format: {trimmed}"))?;
        let _port = parts
            .next()
            .with_context(|| format!("invalid split-tunnel rule format: {trimmed}"))?;
        if parts.next().is_some() {
            bail!("invalid split-tunnel rule format: {trimmed}");
        }
        target.trim()
    } else {
        trimmed
    };

    let (address, netmask) = match route_target.split_once('/') {
        Some((address, suffix)) => {
            let address = match address.parse::<std::net::IpAddr>() {
                Ok(std::net::IpAddr::V4(address)) => address,
                Ok(std::net::IpAddr::V6(_)) => return Ok(None),
                Err(_) => {
                    bail!("invalid split-tunnel IP address: {route_target}");
                }
            };
            let netmask = match suffix.parse::<u8>() {
                Ok(prefix) => prefix_to_netmask(prefix)?,
                Err(_) => suffix.parse::<std::net::Ipv4Addr>().with_context(|| {
                    format!("invalid split-tunnel IPv4 netmask suffix: {route_target}")
                })?,
            };
            (address, netmask)
        }
        None => {
            let address = match route_target.parse::<std::net::IpAddr>() {
                Ok(std::net::IpAddr::V4(address)) => address,
                Ok(std::net::IpAddr::V6(_)) => return Ok(None),
                Err(_) => {
                    bail!("invalid split-tunnel IP host route: {route_target}");
                }
            };
            (address, std::net::Ipv4Addr::new(255, 255, 255, 255))
        }
    };

    Ok(Some(WindowsRoute {
        destination: normalize_network(address, netmask),
        netmask,
    }))
}

fn route_contains(route: WindowsRoute, address: std::net::Ipv4Addr) -> bool {
    normalize_network(address, route.netmask) == route.destination
}

fn exclude_proxy_peer(routes: &mut Vec<WindowsRoute>) {
    let Some(std::net::IpAddr::V4(peer)) = service::proxy_peer_ip() else {
        return;
    };
    routes.retain(|route| {
        if route_contains(*route, peer) {
            warn!(%peer, route = %route.destination, netmask = %route.netmask,
                "excluding proxy endpoint from split-tunnel routes");
            false
        } else {
            true
        }
    });
}

#[cfg(target_os = "windows")]
fn run_route_command(args: &[String]) -> Result<()> {
    let output = std::process::Command::new("route")
        .args(args)
        .output()
        .with_context(|| format!("failed to run route command: {:?}", args))?;
    if output.status.success() {
        return Ok(());
    }

    bail!(
        "route command failed: args={:?}, stdout={}, stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(target_os = "windows")]
fn add_split_tunnel_route(
    route: WindowsRoute,
    gateway: std::net::Ipv4Addr,
    if_index: u32,
) -> Result<()> {
    run_route_command(&[
        String::from("ADD"),
        route.destination.to_string(),
        String::from("MASK"),
        route.netmask.to_string(),
        gateway.to_string(),
        String::from("IF"),
        if_index.to_string(),
    ])
    .with_context(|| {
        format!(
            "failed to add split-tunnel route {}/{} via {} on interface {}",
            route.destination, route.netmask, gateway, if_index
        )
    })?;
    info!(
        destination = %route.destination,
        netmask = %route.netmask,
        gateway = %gateway,
        if_index,
        "installed split-tunnel route"
    );
    Ok(())
}

#[cfg(target_os = "windows")]
fn delete_split_tunnel_route(
    route: WindowsRoute,
    gateway: std::net::Ipv4Addr,
    if_index: u32,
) -> Result<()> {
    run_route_command(&[
        String::from("DELETE"),
        route.destination.to_string(),
        String::from("MASK"),
        route.netmask.to_string(),
        gateway.to_string(),
        String::from("IF"),
        if_index.to_string(),
    ])
    .with_context(|| {
        format!(
            "failed to delete split-tunnel route {}/{} via {} on interface {}",
            route.destination, route.netmask, gateway, if_index
        )
    })?;
    info!(
        destination = %route.destination,
        netmask = %route.netmask,
        gateway = %gateway,
        if_index,
        "removed split-tunnel route"
    );
    Ok(())
}

#[cfg(target_os = "windows")]
fn configure_split_tunnel_routes(
    device: &tun_rs::SyncDevice,
    server: &cczuvpnproto::types::ProxyServer,
) -> Result<Vec<WindowsRoute>> {
    let gateway = server
        .gateway
        .parse::<std::net::Ipv4Addr>()
        .with_context(|| format!("invalid VPN gateway address: {}", server.gateway))?;
    let if_index = device
        .if_index()
        .context("failed to query TUN interface index")?;

    let local_route = WindowsRoute {
        destination: normalize_network(
            server
                .address
                .parse::<std::net::Ipv4Addr>()
                .with_context(|| format!("invalid VPN address: {}", server.address))?,
            server
                .mask
                .parse::<std::net::Ipv4Addr>()
                .with_context(|| format!("invalid VPN mask: {}", server.mask))?,
        ),
        netmask: server
            .mask
            .parse::<std::net::Ipv4Addr>()
            .with_context(|| format!("invalid VPN mask: {}", server.mask))?,
    };

    let mut routes = Vec::new();
    let mut skipped_route_count = 0usize;
    for route in &server.split_tunnel_routes {
        match parse_split_tunnel_route(route)? {
            Some(route) => routes.push(route),
            None => {
                skipped_route_count += 1;
                warn!(
                    rule = route,
                    "skipping unsupported non-IPv4 split-tunnel rule"
                );
            }
        }
    }

    routes.retain(|route| route != &local_route);
    exclude_proxy_peer(&mut routes);
    routes.sort();
    routes.dedup();

    if skipped_route_count > 0 {
        warn!(
            skipped_route_count,
            "skipped unsupported split-tunnel rules while configuring Windows routes"
        );
    }

    let mut installed_routes = Vec::new();
    for route in &routes {
        if let Err(err) = add_split_tunnel_route(*route, gateway, if_index) {
            for installed_route in installed_routes.iter().rev().copied() {
                let _ = delete_split_tunnel_route(installed_route, gateway, if_index);
            }
            return Err(err);
        }
        installed_routes.push(*route);
    }

    Ok(routes)
}

#[cfg(target_os = "windows")]
fn cleanup_split_tunnel_routes(
    device: &tun_rs::SyncDevice,
    server: &cczuvpnproto::types::ProxyServer,
    routes: &[WindowsRoute],
) -> Result<()> {
    let gateway = server
        .gateway
        .parse::<std::net::Ipv4Addr>()
        .with_context(|| format!("invalid VPN gateway address: {}", server.gateway))?;
    let if_index = device
        .if_index()
        .context("failed to query TUN interface index")?;

    for route in routes {
        delete_split_tunnel_route(*route, gateway, if_index)?;
    }

    Ok(())
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct LinuxNetworkConfig {
    interface: String,
    gateway: std::net::Ipv4Addr,
    routes: Vec<WindowsRoute>,
    active: bool,
}

#[cfg(target_os = "linux")]
impl Drop for LinuxNetworkConfig {
    fn drop(&mut self) {
        if let Err(err) = cleanup_linux_network(self) {
            error!(error = %err, "failed to clean up Linux network configuration on drop");
        }
    }
}

#[cfg(target_os = "linux")]
fn run_linux_command(program: &str, args: &[String]) -> Result<()> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("failed to run {program}: {args:?}"))?;
    if output.status.success() {
        return Ok(());
    }

    bail!(
        "{program} failed: args={args:?}, stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(target_os = "linux")]
fn route_cidr(route: WindowsRoute) -> Result<String> {
    Ok(format!(
        "{}/{}",
        route.destination,
        netmask_to_prefix(route.netmask)?
    ))
}

#[cfg(target_os = "linux")]
fn add_linux_route(
    route: WindowsRoute,
    gateway: std::net::Ipv4Addr,
    interface: &str,
) -> Result<()> {
    let destination = route_cidr(route)?;
    run_linux_command(
        "ip",
        &[
            String::from("-4"),
            String::from("route"),
            String::from("add"),
            destination.clone(),
            String::from("via"),
            gateway.to_string(),
            String::from("dev"),
            String::from(interface),
        ],
    )
    .with_context(|| format!("failed to add split-tunnel route {destination} via {gateway}"))?;
    info!(%destination, %gateway, interface, "installed split-tunnel route");
    Ok(())
}

#[cfg(target_os = "linux")]
fn delete_linux_route(
    route: WindowsRoute,
    gateway: std::net::Ipv4Addr,
    interface: &str,
) -> Result<()> {
    let destination = route_cidr(route)?;
    let route_result = run_linux_command(
        "ip",
        &[
            String::from("-4"),
            String::from("route"),
            String::from("del"),
            destination.clone(),
            String::from("via"),
            gateway.to_string(),
            String::from("dev"),
            String::from(interface),
        ],
    );
    route_result.with_context(|| {
        format!("failed to delete split-tunnel route {destination} via {gateway}")
    })?;
    info!(%destination, %gateway, interface, "removed split-tunnel route");
    Ok(())
}

#[cfg(target_os = "linux")]
const LINUX_POLICY_RULE_ARGS: [&str; 8] = [
    "-4",
    "rule",
    "priority",
    "8000",
    "lookup",
    "main",
    "suppress_prefixlength",
    "0",
];

#[cfg(target_os = "linux")]
const VPN_BOOTSTRAP_ENDPOINTS: [std::net::Ipv4Addr; 2] = [
    std::net::Ipv4Addr::new(211, 65, 64, 100),
    std::net::Ipv4Addr::new(211, 65, 66, 99),
];

fn bootstrap_policy_rule_args(action: &str, endpoint: std::net::Ipv4Addr) -> [String; 9] {
    [
        String::from("-4"),
        String::from("rule"),
        String::from(action),
        String::from("priority"),
        String::from("7999"),
        String::from("to"),
        format!("{endpoint}/32"),
        String::from("lookup"),
        String::from("main"),
    ]
}

#[cfg(target_os = "linux")]
fn delete_bootstrap_rule_if_present(endpoint: std::net::Ipv4Addr) -> Result<bool> {
    let args = bootstrap_policy_rule_args("del", endpoint);
    let output = Command::new("ip")
        .args(&args)
        .output()
        .with_context(|| format!("failed to remove bootstrap route for {endpoint}"))?;
    if output.status.success() {
        return Ok(true);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("No such file") {
        return Ok(false);
    }
    bail!("ip failed while removing bootstrap route for {endpoint}: {stderr}")
}

#[cfg(target_os = "linux")]
struct LinuxBootstrapRoutes {
    installed: Vec<std::net::Ipv4Addr>,
}

#[cfg(target_os = "linux")]
impl LinuxBootstrapRoutes {
    fn install() -> Result<Self> {
        let mut guard = Self {
            installed: Vec::new(),
        };
        for endpoint in VPN_BOOTSTRAP_ENDPOINTS {
            while delete_bootstrap_rule_if_present(endpoint)? {
                warn!(%endpoint, "removed a stale VPN bootstrap rule");
            }
            run_linux_command("ip", &bootstrap_policy_rule_args("add", endpoint))
                .with_context(|| format!("failed to bypass Clash TUN for {endpoint}"))?;
            guard.installed.push(endpoint);
        }
        Ok(guard)
    }
}

#[cfg(target_os = "linux")]
impl Drop for LinuxBootstrapRoutes {
    fn drop(&mut self) {
        for endpoint in self.installed.drain(..).rev() {
            if let Err(err) = delete_bootstrap_rule_if_present(endpoint) {
                error!(%endpoint, error = %err, "failed to remove VPN bootstrap rule");
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn delete_linux_policy_rule_if_present() -> Result<bool> {
    let mut args = vec![
        String::from("-4"),
        String::from("rule"),
        String::from("del"),
    ];
    args.extend(
        LINUX_POLICY_RULE_ARGS[2..]
            .iter()
            .map(|value| String::from(*value)),
    );
    let output = Command::new("ip")
        .args(&args)
        .output()
        .context("failed to inspect/remove the CCZU policy rule")?;
    if output.status.success() {
        return Ok(true);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("No such file") {
        return Ok(false);
    }
    bail!("ip failed while removing the CCZU policy rule: {stderr}")
}

#[cfg(target_os = "linux")]
fn install_linux_policy_rule() -> Result<()> {
    while delete_linux_policy_rule_if_present()? {
        warn!("removed a stale CCZU VPN policy rule");
    }
    let mut args = vec![
        String::from("-4"),
        String::from("rule"),
        String::from("add"),
    ];
    args.extend(
        LINUX_POLICY_RULE_ARGS[2..]
            .iter()
            .map(|value| String::from(*value)),
    );
    run_linux_command("ip", &args).context("failed to install the CCZU VPN policy rule")
}

#[cfg(target_os = "linux")]
fn configure_linux_dns(interface: &str, dns: &str) -> Result<()> {
    let dns = dns
        .parse::<std::net::IpAddr>()
        .with_context(|| format!("invalid VPN DNS address: {dns}"))?;
    run_linux_command(
        "resolvectl",
        &[
            String::from("dns"),
            String::from(interface),
            dns.to_string(),
        ],
    )?;
    if let Err(err) = run_linux_command(
        "resolvectl",
        &[
            String::from("domain"),
            String::from(interface),
            String::from("~cczu.edu.cn"),
        ],
    ) {
        let _ = run_linux_command(
            "resolvectl",
            &[String::from("revert"), String::from(interface)],
        );
        return Err(err).context("failed to route CCZU DNS queries through the VPN interface");
    }
    info!(%dns, interface, "configured VPN DNS with systemd-resolved");
    Ok(())
}

#[cfg(target_os = "linux")]
fn configure_linux_network(
    device: &tun_rs::SyncDevice,
    server: &cczuvpnproto::types::ProxyServer,
) -> Result<LinuxNetworkConfig> {
    let interface = device
        .name()
        .context("failed to query TUN interface name")?;
    let gateway = server
        .gateway
        .parse::<std::net::Ipv4Addr>()
        .with_context(|| format!("invalid VPN gateway address: {}", server.gateway))?;
    let local_route = WindowsRoute {
        destination: normalize_network(
            server
                .address
                .parse::<std::net::Ipv4Addr>()
                .with_context(|| format!("invalid VPN address: {}", server.address))?,
            server
                .mask
                .parse::<std::net::Ipv4Addr>()
                .with_context(|| format!("invalid VPN mask: {}", server.mask))?,
        ),
        netmask: server
            .mask
            .parse::<std::net::Ipv4Addr>()
            .with_context(|| format!("invalid VPN mask: {}", server.mask))?,
    };

    let mut routes = Vec::new();
    let mut skipped_route_count = 0usize;
    for rule in &server.split_tunnel_routes {
        match parse_split_tunnel_route(rule)? {
            Some(route) => routes.push(route),
            None => {
                skipped_route_count += 1;
                warn!(rule, "skipping unsupported non-IPv4 split-tunnel rule");
            }
        }
    }
    match server.dns.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(dns)) => routes.push(WindowsRoute {
            destination: dns,
            netmask: std::net::Ipv4Addr::new(255, 255, 255, 255),
        }),
        Ok(std::net::IpAddr::V6(_)) => {
            warn!(dns = %server.dns, "VPN DNS is IPv6; no IPv4 policy route was installed");
        }
        Err(err) => return Err(err).context(format!("invalid VPN DNS address: {}", server.dns)),
    }
    routes.retain(|route| route != &local_route);
    exclude_proxy_peer(&mut routes);
    routes.sort();
    routes.dedup();

    if skipped_route_count > 0 {
        warn!(
            skipped_route_count,
            "skipped unsupported split-tunnel rules while configuring Linux routes"
        );
    }

    let mut installed_routes = Vec::new();
    for route in routes {
        if let Err(err) = add_linux_route(route, gateway, &interface) {
            for installed_route in installed_routes.iter().rev().copied() {
                let _ = delete_linux_route(installed_route, gateway, &interface);
            }
            return Err(err);
        }
        installed_routes.push(route);
    }

    if let Err(err) = configure_linux_dns(&interface, &server.dns) {
        for route in installed_routes.iter().rev().copied() {
            let _ = delete_linux_route(route, gateway, &interface);
        }
        return Err(err);
    }

    if let Err(err) = install_linux_policy_rule() {
        let _ = run_linux_command("resolvectl", &[String::from("revert"), interface.clone()]);
        for route in installed_routes.iter().rev().copied() {
            let _ = delete_linux_route(route, gateway, &interface);
        }
        return Err(err);
    }

    Ok(LinuxNetworkConfig {
        interface,
        gateway,
        routes: installed_routes,
        active: true,
    })
}

#[cfg(target_os = "linux")]
fn cleanup_linux_network(config: &mut LinuxNetworkConfig) -> Result<()> {
    if !config.active {
        return Ok(());
    }
    config.active = false;
    let mut route_error = delete_linux_policy_rule_if_present().err();
    for route in config.routes.iter().rev().copied() {
        if let Err(err) = delete_linux_route(route, config.gateway, &config.interface) {
            error!(error = %err, "failed to remove split-tunnel route during cleanup");
            if route_error.is_none() {
                route_error = Some(err);
            }
        }
    }
    let dns_result = run_linux_command(
        "resolvectl",
        &[String::from("revert"), config.interface.clone()],
    )
    .context("failed to remove VPN DNS configuration");

    if let Some(err) = route_error {
        return Err(err);
    }
    dns_result
}

#[cfg(target_os = "linux")]
async fn wait_for_shutdown_signal() -> Result<&'static str> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut terminate = signal(SignalKind::terminate()).context("failed to listen for SIGTERM")?;
    let mut hangup = signal(SignalKind::hangup()).context("failed to listen for SIGHUP")?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => {
            result.context("failed to listen for ctrl+c")?;
            Ok("ctrl+c")
        }
        _ = terminate.recv() => Ok("SIGTERM"),
        _ = hangup.recv() => Ok("SIGHUP"),
    }
}

#[cfg(not(target_os = "linux"))]
async fn wait_for_shutdown_signal() -> Result<&'static str> {
    tokio::signal::ctrl_c()
        .await
        .context("failed to listen for ctrl+c")?;
    Ok("ctrl+c")
}

async fn create_device() -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        ensure_wintun_dll()?;
    }

    let server = service::proxy_server()
        .ok_or_else(|| std::io::Error::other("No server to create TUN device"))?;
    info!(?server, "creating tun device");

    #[cfg(target_os = "windows")]
    let destination = Some(server.gateway.as_str());
    #[cfg(not(target_os = "windows"))]
    let destination: Option<&str> = None;

    let builder = DeviceBuilder::new().name("CCZU-VPN-PROTO").ipv4(
        server.address.as_str(),
        server.mask.as_str(),
        destination,
    );

    #[cfg(target_os = "windows")]
    let builder = builder
        .description("CCZU-VPN-PROTO")
        .wintun_file(String::from("wintun.dll"));

    let device = builder.build_sync()?;

    #[cfg(target_os = "windows")]
    {
        let dns_server = server.dns.parse::<std::net::IpAddr>()?;
        device.set_dns_servers(&[dns_server])?;
    }

    #[cfg(target_os = "windows")]
    let split_tunnel_routes = configure_split_tunnel_routes(&device, &server)?;

    #[cfg(target_os = "linux")]
    let mut linux_network = configure_linux_network(&device, &server)?;

    let device = Arc::new(device);
    let device_output = device.clone();
    let interrupt_event = match InterruptEvent::new() {
        Ok(event) => Arc::new(event),
        Err(err) => {
            #[cfg(target_os = "windows")]
            let _ = cleanup_split_tunnel_routes(device.as_ref(), &server, &split_tunnel_routes);
            #[cfg(target_os = "linux")]
            let _ = cleanup_linux_network(&mut linux_network);
            return Err(err.into());
        }
    };
    let shutdown_requested = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let ctrl_c_interrupt_event = interrupt_event.clone();
    let ctrl_c_shutdown_requested = shutdown_requested.clone();
    let shutdown_task = tokio::spawn(async move {
        let signal = wait_for_shutdown_signal().await?;
        info!(signal, "received shutdown signal, starting shutdown");
        ctrl_c_shutdown_requested.store(true, std::sync::atomic::Ordering::Relaxed);
        service::stop_polling_packet();
        ctrl_c_interrupt_event
            .trigger()
            .context("failed to trigger tun interrupt event")?;
        Ok::<(), anyhow::Error>(())
    });

    if let Err(err) = service::start_polling_packet(move |a, b| {
        debug!(packet_size = a, "received packet from proxy");
        if let Err(err) = device_output.send(&b) {
            error!(packet_size = a, error = %err, "failed to write packet to TUN device");
        }
    }) {
        shutdown_task.abort();
        #[cfg(target_os = "windows")]
        let _ = cleanup_split_tunnel_routes(device.as_ref(), &server, &split_tunnel_routes);
        #[cfg(target_os = "linux")]
        let _ = cleanup_linux_network(&mut linux_network);
        return Err(err);
    }

    let loop_result: Result<()> = async {
        let mut buf = [0; 65535];
        loop {
            let len = match device.recv_intr(&mut buf, interrupt_event.as_ref()) {
                Ok(len) => len,
                Err(err)
                    if err.kind() == std::io::ErrorKind::Interrupted
                        && shutdown_requested.load(std::sync::atomic::Ordering::Relaxed) =>
                {
                    info!("tun read interrupted for shutdown");
                    break;
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {
                    warn!(error = %err, "tun read interrupted unexpectedly");
                    continue;
                }
                Err(err) => return Err(err.into()),
            };
            if len >= 20 && buf[0] >> 4 == 4 {
                debug!(
                    packet_size = len,
                    protocol = buf[9],
                    source = %std::net::Ipv4Addr::new(buf[12], buf[13], buf[14], buf[15]),
                    destination = %std::net::Ipv4Addr::new(buf[16], buf[17], buf[18], buf[19]),
                    "read IPv4 packet from tun device"
                );
            } else {
                debug!(packet_size = len, "read packet from tun device");
            }
            if let Err(err) = service::send_tcp_packet(&buf[..len]).await {
                error!(packet_size = len, error = %err, "failed to send packet to proxy");
            }

            if service::POLLER_SIGNAL.load(std::sync::atomic::Ordering::Relaxed) {
                return Ok(());
            }
        }
        Ok(())
    }
    .await;

    let stop_result = service::stop_service().await;

    #[cfg(target_os = "windows")]
    let cleanup_result =
        cleanup_split_tunnel_routes(device.as_ref(), &server, &split_tunnel_routes);

    #[cfg(target_os = "linux")]
    let cleanup_result = cleanup_linux_network(&mut linux_network);

    let shutdown_result = if shutdown_requested.load(std::sync::atomic::Ordering::Relaxed) {
        shutdown_task.await.context("shutdown task join failed")?
    } else {
        shutdown_task.abort();
        Ok(())
    };

    loop_result?;
    stop_result?;
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    cleanup_result?;
    shutdown_result?;

    Ok(())
}

pub async fn run() -> Result<()> {
    let args = CliArgs::parse();
    diag::try_init_tracing(args.log_level.as_str());

    if args.forget_login {
        forget_saved_credentials()?;
        println!("已清除系统密钥环中的 CCZU VPN 登录信息。");
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    let _bootstrap_routes = LinuxBootstrapRoutes::install()?;

    if !cczuni::impls::client::DefaultClient::default()
        .webvpn_available()
        .await
    {
        warn!("webvpn availability check failed, asking user whether to continue");
        if !args.yes
            && !confirm_continue("webvpn may not be available, are you sure to connect? (Y/n) ")?
        {
            return Ok(());
        }
    }

    let (credentials, loaded_from_keyring) = resolve_credentials(&args)?;
    info!("starting interactive vpn session");
    service::start_service(credentials.user.clone(), credentials.password.clone()).await?;
    if cfg!(target_os = "linux")
        && !args.no_keyring
        && !loaded_from_keyring
        && confirm_explicitly("登录成功。将账号和密码保存到系统密钥环？(y/N) ")?
    {
        match save_credentials(&credentials) {
            Ok(()) => println!("登录信息已保存到系统密钥环。"),
            Err(err) => warn!(error = %err, "登录成功，但无法保存到系统密钥环"),
        }
    }
    let result = create_device().await;
    if result.is_err()
        && service::service_available().await
        && let Err(err) = service::stop_service().await
    {
        error!(error = %err, "failed to stop VPN service after device setup failure");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{
        SavedCredentials, bootstrap_policy_rule_args, netmask_to_prefix, parse_split_tunnel_route,
        route_contains,
    };
    use std::net::Ipv4Addr;

    #[test]
    fn parses_and_normalizes_split_tunnel_rule() {
        let route = parse_split_tunnel_route("tcp;10.22.1.99/24;443")
            .expect("route should parse")
            .expect("IPv4 route should be retained");

        assert_eq!(route.destination, Ipv4Addr::new(10, 22, 1, 0));
        assert_eq!(route.netmask, Ipv4Addr::new(255, 255, 255, 0));
    }

    #[test]
    fn parses_host_routes_and_skips_ipv6_rules() {
        let route = parse_split_tunnel_route("10.22.1.99")
            .expect("route should parse")
            .expect("IPv4 route should be retained");
        assert_eq!(route.netmask, Ipv4Addr::new(255, 255, 255, 255));

        assert_eq!(
            parse_split_tunnel_route("2001:db8::1/64").expect("IPv6 rule should parse"),
            None
        );
    }

    #[test]
    fn rejects_non_contiguous_netmasks() {
        assert!(netmask_to_prefix(Ipv4Addr::new(255, 0, 255, 0)).is_err());
    }

    #[test]
    fn detects_proxy_inside_split_tunnel_route() {
        let route = parse_split_tunnel_route("211.65.64.0/20")
            .expect("route should parse")
            .expect("IPv4 route should be retained");
        assert!(route_contains(route, Ipv4Addr::new(211, 65, 64, 100)));
        assert!(!route_contains(route, Ipv4Addr::new(211, 65, 80, 1)));
    }

    #[test]
    fn saved_credentials_debug_redacts_password() {
        let credentials = SavedCredentials {
            user: String::from("student"),
            password: String::from("secret-password"),
        };
        let debug = format!("{credentials:?}");
        assert!(debug.contains("student"));
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("secret-password"));
    }

    #[test]
    fn bootstrap_rule_only_bypasses_one_school_endpoint() {
        assert_eq!(
            bootstrap_policy_rule_args("add", Ipv4Addr::new(211, 65, 66, 99)),
            [
                "-4",
                "rule",
                "add",
                "priority",
                "7999",
                "to",
                "211.65.66.99/32",
                "lookup",
                "main",
            ]
        );
    }
}
