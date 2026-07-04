use crate::metamask_login::{build_login_eip712, LoginDomain, LoginMessage};
use mm2_err_handle::prelude::*;
use mm2_eth::keys::{address_from_uncompressed_pubkey, recover_public_key, Address, Signature, H520};
use mm2_metamask::{Eip1193Provider, MetamaskSession};
use std::ops::Deref;
use std::str::FromStr;
use std::sync::{Arc, Weak};

pub use mm2_metamask::{MetamaskError, MetamaskResult};

#[derive(Clone)]
pub struct MetamaskArc(Arc<MetamaskCtx>);

impl MetamaskArc {
    pub fn new(ctx: MetamaskCtx) -> Self { MetamaskArc(Arc::new(ctx)) }

    pub fn downgrade(&self) -> MetamaskWeak { MetamaskWeak(Arc::downgrade(&self.0)) }
}

impl Deref for MetamaskArc {
    type Target = MetamaskCtx;

    fn deref(&self) -> &Self::Target { &self.0 }
}

#[derive(Clone)]
pub struct MetamaskWeak(Weak<MetamaskCtx>);

impl MetamaskWeak {
    pub fn upgrade(&self) -> Option<MetamaskArc> { self.0.upgrade().map(MetamaskArc) }
}

pub struct MetamaskCtx {
    eth_account: Address,
    eth_account_str: String,
    /// Full uncompressed public key (65 bytes, 0x04 prefix).
    eth_account_pubkey: H520,
    eip_provider: Eip1193Provider,
}

impl MetamaskCtx {
    /// Detects MetaMask, requests the active account, signs a login
    /// challenge and recovers the public key to verify ownership.
    pub async fn init(project_name: String) -> MetamaskResult<Self> {
        let eip_provider = Eip1193Provider::detect().or_mm_err(|| MetamaskError::EthProviderNotFound)?;

        let (eth_account, eth_account_str, eth_account_pubkey) = {
            let session = MetamaskSession::lock(&eip_provider).await;
            let acct_str = session.eth_request_account().await?;
            let acct = Address::from_str(&acct_str)
                .map_to_mm(|e| MetamaskError::ErrorDeserializingMethodResult(e.to_string()))?;

            let domain = LoginDomain::new(project_name.clone());
            let message = LoginMessage::new(project_name);
            let (hash_bytes, sig_hex) = session
                .sign_typed_data_v4(acct_str.clone(), build_login_eip712(domain, message))
                .await?;

            // Parse the hexadecimal signature (may have 0x prefix).
            let sig_str = sig_hex.strip_prefix("0x").unwrap_or(&sig_hex);
            let signature = Signature::from_str(sig_str)
                .map_to_mm(|_| MetamaskError::Internal(format!("invalid signature: '{sig_str}'")))?;

            // Convert the raw [u8; 32] hash to ethereum_types::H256 for recovery.
            let hash_h256 = mm2_eth::keys::H256::from(hash_bytes);
            let pubkey = recover_public_key(hash_h256, signature).mm_err(|_| {
                MetamaskError::Internal(format!("could not recover public key from signature '{sig_str}'"))
            })?;

            let recovered = address_from_uncompressed_pubkey(pubkey);
            if acct != recovered {
                return MmError::err(MetamaskError::Internal(format!(
                    "recovered address '{recovered:?}' does not match '{acct:?}'"
                )));
            }

            (acct, acct_str, pubkey)
        };

        Ok(MetamaskCtx {
            eth_account,
            eth_account_str,
            eth_account_pubkey,
            eip_provider,
        })
    }

    #[inline]
    pub fn eth_account(&self) -> Address { self.eth_account }

    #[inline]
    pub fn eth_account_str(&self) -> &str { &self.eth_account_str }

    #[inline]
    pub fn eth_account_pubkey_uncompressed(&self) -> H520 { self.eth_account_pubkey }

    /// Returns a reference to the underlying EIP-1193 transport.
    #[inline]
    pub fn eip_provider(&self) -> &Eip1193Provider { &self.eip_provider }

    /// Verifies that `eth_account` is still the active MetaMask account.
    pub async fn check_active_eth_account(&self) -> MetamaskResult<&Address> {
        let current = self.current_eth_account().await?;
        if current == self.eth_account_str {
            Ok(&self.eth_account)
        } else {
            MmError::err(MetamaskError::UnexpectedAccountSelected)
        }
    }

    /// Returns the currently active ETH account from MetaMask.
    pub async fn current_eth_account(&self) -> MetamaskResult<String> {
        let session = MetamaskSession::lock(&self.eip_provider).await;
        session.eth_request_account().await
    }

    /// Hands an unsigned EIP-1193 transaction object to the wallet to **sign and
    /// broadcast** via `eth_sendTransaction`, returning the broadcast
    /// transaction hash (CRD R47.5.6 / R47.5.8). The framework holds no key.
    pub async fn eth_send_transaction(&self, tx: serde_json::Value) -> MetamaskResult<String> {
        let session = MetamaskSession::lock(&self.eip_provider).await;
        session.eth_send_transaction(tx).await
    }

    /// Ensures the wallet's active EIP-155 chain matches `chain_id`, requesting
    /// a `wallet_switchEthereumChain` switch otherwise (CRD R47.5.10). A
    /// rejected/failed switch surfaces as an error so the caller never
    /// broadcasts on the wrong chain.
    pub async fn ensure_active_chain(&self, chain_id: u64) -> MetamaskResult<()> {
        let session = MetamaskSession::lock(&self.eip_provider).await;
        let active_chain_hex = session.eth_chain_id().await?;
        let active_chain_id = u64::from_str_radix(active_chain_hex.trim_start_matches("0x"), 16)
            .map_to_mm(|e| MetamaskError::Internal(format!("invalid chainId '{active_chain_hex}': {e}")))?;
        if active_chain_id != chain_id {
            session.wallet_switch_ethereum_chain(chain_id).await?;
        }
        Ok(())
    }
}
