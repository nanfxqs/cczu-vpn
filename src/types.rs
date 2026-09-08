use serde::Serialize;

#[derive(Debug, Serialize, Clone, uniffi::Record)]
pub struct ProxyServer {
    pub address: String,
    pub mask: String,
    pub gateway: String,
    pub dns: String,
    pub wins: String,
    pub split_tunnel_routes: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, uniffi::Record)]
pub struct StartOptions {
    pub no_verification: bool,
}
