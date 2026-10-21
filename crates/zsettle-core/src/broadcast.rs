//! Talking to a lightwalletd server: broadcasting a finished transaction, fetching the
//! commitment-tree state needed to import a viewing key, and checking that the server's
//! chain is the one this build signs for.

use anyhow::{anyhow, Result};
use std::task::{Context, Poll};

use tonic::body::Body;
use tonic::codegen::{http, BoxFuture, Service};
use tonic::transport::{Channel, ClientTlsConfig};
use zcash_client_backend::proto::service::{
    compact_tx_streamer_client::CompactTxStreamerClient, BlockId, ChainSpec, Empty, RawTransaction,
    TreeState,
};
use zcash_protocol::consensus::{BlockHeight, BranchId, Network};

/// Default mainnet lightwalletd endpoint.
pub const DEFAULT_SERVER: &str = "https://zec.rocks:443";

/// Whether this build syncs the testnet. The main line talks to mainnet servers only; the
/// nu7-testnet line picks a server per network.
pub const TESTNET_BUILD: bool = false;

/// The one lightwalletd method that would carry the wallet's transparent addresses.
const ADDRESS_UTXOS: &str = "/cash.z.wallet.sdk.rpc.CompactTxStreamer/GetAddressUtxosStream";

/// A lightwalletd channel that never sends transparent addresses. The wallet library's sync
/// refreshes transparent UTXOs through `GetAddressUtxosStream` whenever its PCZT support is
/// compiled in, which would hand the server a persistent identifier of the wallet on every
/// pass. zsettle holds no transparent funds, so that request is answered here with an empty
/// result and never leaves the process.
#[derive(Clone)]
pub struct Shielded<S>(S);

impl<S> Service<http::Request<Body>> for Shielded<S>
where
    S: Service<http::Request<Body>, Response = http::Response<Body>>,
    S::Future: Send + 'static,
    S::Error: Send + 'static,
{
    type Response = http::Response<Body>;
    type Error = S::Error;
    type Future = BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.0.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<Body>) -> Self::Future {
        // The client prefixes the method path with the server URL's path, if any.
        if req.uri().path().ends_with(ADDRESS_UTXOS) {
            // A gRPC "trailers-only" response: status OK and no messages.
            let resp = http::Response::builder()
                .status(200)
                .header("content-type", "application/grpc")
                .header("grpc-status", "0")
                .body(Body::default())
                .expect("static response");
            return Box::pin(async move { Ok(resp) });
        }
        Box::pin(self.0.call(req))
    }
}

/// The client every zsettle command talks through.
pub type Client = CompactTxStreamerClient<Shielded<Channel>>;

/// Open a gRPC channel to a lightwalletd server (TLS with WebPKI roots for `https` URLs),
/// wrapped so that transparent addresses never go out.
pub async fn connect(server: &str) -> Result<Client> {
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
    Ok(CompactTxStreamerClient::new(Shielded(channel)))
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

/// The server's chain tip, and the hash of the block at `height` as block explorers show it
/// (`None` when the server does not have that block yet).
pub async fn block_hash_at(server: &str, height: u32) -> Result<(u32, Option<String>)> {
    let mut client = connect(server).await?;
    let tip = client
        .get_latest_block(ChainSpec {})
        .await
        .map_err(|e| anyhow!("get_latest_block: {e}"))?
        .into_inner();
    let tip = u32::try_from(tip.height).map_err(|_| anyhow!("height out of range"))?;
    if height > tip {
        return Ok((tip, None));
    }
    let block = client
        .get_block(BlockId {
            height: u64::from(height),
            hash: vec![],
        })
        .await
        .map_err(|e| anyhow!("get_block({height}): {e}"))?
        .into_inner();
    let hash = zcash_primitives::block::BlockHash::try_from_slice(&block.hash)
        .ok_or_else(|| anyhow!("the server sent a malformed hash for block {height}"))?;
    Ok((tip, Some(hash.to_string())))
}

/// Blocking wrapper around [`block_hash_at`].
pub fn block_hash_at_blocking(server: &str, height: u32) -> Result<(u32, Option<String>)> {
    runtime()?.block_on(block_hash_at(server, height))
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
    check_consensus_info(server, &info, network, height)?;
    Ok(info)
}

/// The same check at the server's next block, in one round trip.
pub fn check_consensus_at_tip_blocking(server: &str, network: Network) -> Result<ServerInfo> {
    let info = server_info_blocking(server)?;
    check_consensus_info(server, &info, network, info.block_height.saturating_add(1))?;
    Ok(info)
}

fn check_consensus_info(
    server: &str,
    info: &ServerInfo,
    network: Network,
    height: u32,
) -> Result<()> {
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An inner service that records which path reached it.
    struct Inner;

    impl Service<http::Request<Body>> for Inner {
        type Response = http::Response<Body>;
        type Error = std::convert::Infallible;
        type Future = BoxFuture<Self::Response, Self::Error>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: http::Request<Body>) -> Self::Future {
            let path = req.uri().path().to_string();
            Box::pin(async move {
                Ok(http::Response::builder()
                    .header("x-reached-server", path)
                    .body(Body::default())
                    .unwrap())
            })
        }
    }

    #[test]
    fn transparent_address_requests_never_reach_the_server() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut s = Shielded(Inner);
            let req = http::Request::builder()
                .uri(ADDRESS_UTXOS)
                .body(Body::default())
                .unwrap();
            let resp = s.call(req).await.unwrap();
            assert_eq!(resp.headers().get("grpc-status").unwrap(), "0");
            assert!(resp.headers().get("x-reached-server").is_none());

            // A server behind a path prefix: the method path is appended to it.
            let prefixed = format!("/lwd{ADDRESS_UTXOS}");
            let req = http::Request::builder()
                .uri(prefixed.as_str())
                .body(Body::default())
                .unwrap();
            let resp = s.call(req).await.unwrap();
            assert_eq!(resp.headers().get("grpc-status").unwrap(), "0");
            assert!(resp.headers().get("x-reached-server").is_none());

            let other = "/cash.z.wallet.sdk.rpc.CompactTxStreamer/GetLatestBlock";
            let req = http::Request::builder()
                .uri(other)
                .body(Body::default())
                .unwrap();
            let resp = s.call(req).await.unwrap();
            assert_eq!(resp.headers().get("x-reached-server").unwrap(), other);
        });
    }
}
