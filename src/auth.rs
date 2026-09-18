use std::{collections::HashMap, future::Future, time::Duration};

use aes::{
    Aes128Enc,
    cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7},
};
use anyhow::{Context, Result, bail};
use base64::{Engine, prelude::BASE64_STANDARD};
use cbc::Encryptor;
use cczuni::base::client::Client;
use cczuni::impls::{
    client::DefaultClient,
    login::sso_type::ElinkLoginInfo,
    services::{
        webvpn::WebVPNService,
        webvpn_type::{ElinkProxyData, Message},
    },
};
use rand::Rng;

const WEBVPN_ROOT: &str = "https://zmvpn.cczu.edu.cn";

fn webvpn_login_form(user: String, password: String) -> Result<HashMap<&'static str, String>> {
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut token = {
        let mut rng = rand::rng();
        (0..16)
            .map(|_| CHARSET[rng.random_range(0..CHARSET.len())])
            .collect::<Vec<u8>>()
    };
    let iv = token.clone();
    token.reverse();
    let encryptor = Encryptor::<Aes128Enc>::new(token.as_slice().into(), iv.as_slice().into());
    let mut buffer = vec![0; password.len() + 16];
    buffer[..password.len()].copy_from_slice(password.as_bytes());
    let encrypted_password = encryptor
        .encrypt_padded_mut::<Pkcs7>(&mut buffer, password.len())
        .map_err(|_| anyhow::anyhow!("password encryption failed"))?;

    Ok(HashMap::from([
        ("username", user),
        ("password", BASE64_STANDARD.encode(encrypted_password)),
        (
            "token",
            token.into_iter().map(char::from).collect::<String>(),
        ),
        ("language", String::from("zh-CN,zh;q=0.9,en;q=0.8")),
    ]))
}

async fn webvpn_login(client: &DefaultClient) -> Result<ElinkLoginInfo> {
    let account = client.account();
    let form = webvpn_login_form(account.user, account.password)?;
    let response = client
        .reqwest_client()
        .post(format!("{WEBVPN_ROOT}/enlink/sso/login/submit"))
        .header("Referer", format!("{WEBVPN_ROOT}/enlink/sso/login"))
        .header("Origin", WEBVPN_ROOT)
        .form(&form)
        .send()
        .await
        .context("failed to send WebVPN login request")?;

    if !response.status().is_redirection() {
        bail!("WebVPN login failed with status: {}", response.status());
    }
    let client_info = response
        .cookies()
        .find(|cookie| cookie.name() == "clientInfo")
        .context("WebVPN login response did not contain clientInfo")?;
    let decoded = BASE64_STANDARD
        .decode(client_info.value())
        .context("failed to decode WebVPN clientInfo")?;
    serde_json::from_slice(&decoded).context("failed to parse WebVPN clientInfo")
}

pub async fn authorize(
    user: impl Into<String>,
    password: impl Into<String>,
) -> Result<Message<ElinkProxyData>> {
    run_with_timeout(Duration::from_secs(30), async {
        let client = DefaultClient::account(user, password);
        let info = webvpn_login(&client).await?;
        client.webvpn_get_proxy_service(info.userid).await
    })
    .await?
}

async fn run_with_timeout<T>(duration: Duration, operation: impl Future<Output = T>) -> Result<T> {
    tokio::time::timeout(duration, operation)
        .await
        .context("authorization timed out")
}

#[cfg(test)]
mod tests {
    use std::{future::pending, time::Duration};

    use super::run_with_timeout;

    #[tokio::test]
    async fn authorization_operation_cannot_wait_forever() {
        let result = run_with_timeout(Duration::from_millis(1), pending::<()>()).await;

        assert!(result.is_err());
        assert!(
            result
                .expect_err("pending authorization must time out")
                .to_string()
                .contains("timed out")
        );
    }
}
