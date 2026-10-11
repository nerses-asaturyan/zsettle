//! Talking to a lightwalletd server: broadcasting a finished transaction and fetching the
//! commitment-tree state needed to import a viewing key.

use anyhow::{anyhow, Result};
use tonic::transport::{Channel, ClientTlsConfig};
use zcash_client_backend::proto::service::{
    compact_tx_streamer_client::CompactTxStreamerClient, BlockId, RawTransaction, TreeState,
};

/// Default mainnet lightwalletd endpoint.
pub const DEFAULT_SERVER: &str = "https://zec.rocks:443";

async fn connect(server: &str) -> Result<CompactTxStreamerClient<Channel>> {
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
    Ok(CompactTxStreamerClient::new(channel))
}

fn install_crypto_provider() {
    // rustls needs a process-wide crypto provider; installing twice is harmless.
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Send raw transaction bytes to a lightwalletd server. Returns the server's message.
pub async fn send_raw_transaction(server: &str, tx_bytes: Vec<u8>) -> Result<String> {
    let mut client = connect(server).await?;
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
    install_crypto_provider();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(send_raw_transaction(server, tx_bytes))
}

/// Fetch the commitment-tree state at a height (used as an account birthday).
pub async fn fetch_tree_state(server: &str, height: u32) -> Result<TreeState> {
    let mut client = connect(server).await?;
    let ts = client
        .get_tree_state(BlockId {
            height: u64::from(height),
            hash: vec![],
        })
        .await
        .map_err(|e| anyhow!("get_tree_state({height}): {e}"))?
        .into_inner();
    Ok(ts)
}

/// Blocking wrapper around [`fetch_tree_state`].
pub fn fetch_tree_state_blocking(server: &str, height: u32) -> Result<TreeState> {
    install_crypto_provider();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(fetch_tree_state(server, height))
}

/// Fetch the latest block height the server knows.
pub fn latest_height_blocking(server: &str) -> Result<u32> {
    install_crypto_provider();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let mut client = connect(server).await?;
            let id = client
                .get_latest_block(zcash_client_backend::proto::service::ChainSpec {})
                .await
                .map_err(|e| anyhow!("get_latest_block: {e}"))?
                .into_inner();
            u32::try_from(id.height).map_err(|_| anyhow!("height out of range"))
        })
}
