//! Broadcasting a finished transaction through a lightwalletd server.

use anyhow::{anyhow, Result};
use tonic::transport::{Channel, ClientTlsConfig};
use zcash_client_backend::proto::service::{
    compact_tx_streamer_client::CompactTxStreamerClient, RawTransaction,
};

/// Default mainnet lightwalletd endpoint.
pub const DEFAULT_SERVER: &str = "https://zec.rocks:443";

/// Send raw transaction bytes to a lightwalletd server. Returns the server's message.
pub async fn send_raw_transaction(server: &str, tx_bytes: Vec<u8>) -> Result<String> {
    let uri: tonic::transport::Uri = server.parse().map_err(|e| anyhow!("server url: {e}"))?;
    let host = uri
        .host()
        .ok_or_else(|| anyhow!("server url has no host"))?
        .to_string();
    let mut endpoint = Channel::builder(uri.clone());
    if uri.scheme_str() == Some("https") {
        endpoint = endpoint
            .tls_config(ClientTlsConfig::new().domain_name(host).with_webpki_roots())
            .map_err(|e| anyhow!("tls config: {e}"))?;
    }
    let channel = endpoint
        .connect()
        .await
        .map_err(|e| anyhow!("connecting to {server}: {e}"))?;
    let mut client = CompactTxStreamerClient::new(channel);
    let response = client
        .send_transaction(RawTransaction {
            data: tx_bytes,
            height: 0,
        })
        .await
        .map_err(|e| anyhow!("send_transaction: {e}"))?
        .into_inner();
    if response.error_code != 0 {
        return Err(anyhow!(
            "server rejected transaction (code {}): {}",
            response.error_code,
            response.error_message
        ));
    }
    Ok(response.error_message)
}

/// Blocking wrapper around [`send_raw_transaction`].
pub fn send_raw_transaction_blocking(server: &str, tx_bytes: Vec<u8>) -> Result<String> {
    // rustls needs a process-wide crypto provider; installing twice is harmless.
    let _ = rustls::crypto::ring::default_provider().install_default();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(send_raw_transaction(server, tx_bytes))
}
