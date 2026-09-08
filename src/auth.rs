use std::{future::Future, time::Duration};

use anyhow::{Context, Result};
use cczuni::impls::{
    client::DefaultClient,
    login::sso::SSOUniversalLogin,
    services::{
        webvpn::WebVPNService,
        webvpn_type::{ElinkProxyData, Message},
    },
};

pub async fn authorize(
    user: impl Into<String>,
    password: impl Into<String>,
) -> Result<Message<ElinkProxyData>> {
    run_with_timeout(Duration::from_secs(30), async {
        let client = DefaultClient::account(user, password);
        if let Some(info) = client.sso_universal_login().await? {
            Ok(client.webvpn_get_proxy_service(info.userid).await?)
        } else {
            Err(anyhow::anyhow!("WebVPN not available"))
        }
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
