//! Talking to a lightwalletd server: broadcasting a finished transaction, fetching the
//! commitment-tree state needed to import a viewing key, and checking that the server's
//! chain is the one this build signs for.

use anyhow::{anyhow, Result};
use tonic::transport::{Channel, ClientTlsConfig};
use zcash_client_backend::proto::service::{
    compact_tx_streamer_client::CompactTxStreamerClient, BlockId, Empty, RawTransaction, TreeState,
};
use zcash_protocol::consensus::{BlockHeight, BranchId, Network};

/// Default mainnet lightwalletd endpoint.
pub const DEFAULT_SERVER: &str = "https://zec.rocks:443";

/// Open a gRPC channel to a lightwalletd server (TLS with WebPKI roots for `https` URLs).
pub async fn connect(server: &str) -> Result<CompactTxStreamerClient<Channel>> {
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

/// Install the process-wide rustls crypto provider. Call before any TLS connection.
pub fn install_crypto_provider() {
    // rustls needs a process-wide crypto provider; installing twice is harmless.
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    install_crypto_provider();
    Ok(tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?)
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
    runtime()?.block_on(send_raw_transaction(server, tx_bytes))
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
    runtime()?.block_on(fetch_tree_state(server, height))
}

/// What a lightwalletd server reports about its chain.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ServerInfo {
    /// `main` or `test`.
    pub chain_name: String,
    /// Consensus branch id currently in force on the server's chain.
    pub consensus_branch_id: u32,
    /// Height of the server's chain tip.
    pub block_height: u32,
}

/// Ask the server which chain and consensus branch it is on.
pub async fn server_info(server: &str) -> Result<ServerInfo> {
    let mut client = connect(server).await?;
    let info = client
        .get_lightd_info(Empty {})
        .await
        .map_err(|e| anyhow!("get_lightd_info: {e}"))?
        .into_inner();
    let branch = info
        .consensus_branch_id
        .trim()
        .trim_start_matches("0x")
        .to_string();
    let consensus_branch_id = u32::from_str_radix(&branch, 16)
        .map_err(|_| anyhow!("server reports an unparseable consensus branch id {branch:?}"))?;
    Ok(ServerInfo {
        chain_name: info.chain_name,
        consensus_branch_id,
        block_height: u32::try_from(info.block_height)
            .map_err(|_| anyhow!("height out of range"))?,
    })
}

/// Blocking wrapper around [`server_info`].
pub fn server_info_blocking(server: &str) -> Result<ServerInfo> {
    runtime()?.block_on(server_info(server))
}

/// The consensus branch this build signs for at `height`.
pub fn local_branch(network: Network, height: u32) -> BranchId {
    BranchId::for_height(&network, BlockHeight::from(height))
}

/// Refuse to build or send a transaction when the server's chain is not the wallet's, or
/// when the server is on a consensus branch this build does not sign for: after a network
/// upgrade the pinned libraries do not know, every transaction they sign is invalid, and
/// it is better to say so than to broadcast one.
pub fn check_consensus_blocking(server: &str, network: Network, height: u32) -> Result<ServerInfo> {
    let info = server_info_blocking(server)?;
    let expected_chain = match network {
        Network::MainNetwork => "main",
        Network::TestNetwork => "test",
    };
    if info.chain_name != expected_chain {
        return Err(anyhow!(
            "the server {server} is on chain {:?} but the wallet is for {expected_chain:?}",
            info.chain_name
        ));
    }
    let ours = local_branch(network, height);
    let ours_id = u32::from(ours);
    if info.consensus_branch_id != ours_id {
        return Err(anyhow!(
            "the server is on consensus branch {:#010x} but this build signs for {:#010x} ({ours:?}) at height {height}: \
             a network upgrade this build does not know is active; update zsettle before trading",
            info.consensus_branch_id,
            ours_id
        ));
    }
    Ok(info)
}
