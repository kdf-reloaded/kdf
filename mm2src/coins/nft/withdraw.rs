//! NFT withdraw implementation for EVM-family chains.
//!
//! Builds a signed (but **not** broadcast) ERC-721 `transferFrom` or
//! ERC-1155 `safeTransferFrom` transaction and returns it as a
//! [`TransactionDetails`] payload. Broadcasting is the caller's
//! responsibility (it goes through `send_raw_transaction` exactly the
//! same way a regular EVM withdraw does), which keeps the RPC surface
//! symmetric with the GUI's existing "build then push" flow for fungible
//! tokens.
//!
//! Only the locally-held key-pair signing path is supported; HD-wallet
//! and hardware-wallet derivations will be wired in alongside the rest
//! of the EVM HD integration.

use crate::eth::{checksum_address, get_addr_nonce, u256_to_big_decimal, wei_from_big_decimal, Action, Address,
                 BytesJson, CallRequest, Contract, EthCoin, EthCoinType, EthTxFeeDetails, Token, TransactionDetails,
                 UnSignedEthTx, NONCE_LOCK, U256};
use crate::nft::errors::GetNftInfoError;
use crate::nft::model::chain::ChainTicker;
use crate::nft::model::{WithdrawErc1155, WithdrawErc721, WithdrawNftReq};
use crate::{lp_coinfind, MarketCoinOps, MmCoinEnum, Transaction, WithdrawFee};
use bigdecimal::BigDecimal;
use common::executor::Timer;
use common::now_ms;
use ethereum_types::H160;
use futures::compat::Future01CompatExt;
use futures::future::{select, Either};
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use mm2_number::BigUint;
use std::str::FromStr;

/// Minimal ERC-721 ABI: only the `transferFrom(address,address,uint256)`
/// entrypoint that we need to build a withdraw calldata.
const ERC721_ABI: &str = r#"[
    {"inputs":[{"name":"from","type":"address"},{"name":"to","type":"address"},{"name":"tokenId","type":"uint256"}],
     "name":"transferFrom","outputs":[],"stateMutability":"nonpayable","type":"function"}
]"#;

/// Minimal ERC-1155 ABI: `safeTransferFrom(address,address,uint256,uint256,bytes)`
/// and `balanceOf(address,uint256)` for the optional `max=true` path.
const ERC1155_ABI: &str = r#"[
    {"inputs":[{"name":"from","type":"address"},{"name":"to","type":"address"},
               {"name":"id","type":"uint256"},{"name":"value","type":"uint256"},
               {"name":"data","type":"bytes"}],
     "name":"safeTransferFrom","outputs":[],"stateMutability":"nonpayable","type":"function"},
    {"inputs":[{"name":"account","type":"address"},{"name":"id","type":"uint256"}],
     "name":"balanceOf","outputs":[{"name":"","type":"uint256"}],"stateMutability":"view","type":"function"}
]"#;

lazy_static::lazy_static! {
    static ref ERC721_CONTRACT: Contract = Contract::load(ERC721_ABI.as_bytes()).expect("ERC-721 ABI is valid");
    static ref ERC1155_CONTRACT: Contract = Contract::load(ERC1155_ABI.as_bytes()).expect("ERC-1155 ABI is valid");
}

/// Convert a `BigUint` token id / amount to a `U256`. Returns an error if
/// the value exceeds 256 bits.
fn biguint_to_u256(value: &BigUint, label: &str) -> Result<U256, GetNftInfoError> {
    let dec = value.to_str_radix(10);
    U256::from_dec_str(&dec).map_err(|_| {
        GetNftInfoError::InvalidRequest(format!("{label} {dec} does not fit into a 256-bit unsigned integer"))
    })
}

/// Parse a hex-prefixed (or bare) Ethereum address.
fn parse_eth_address(text: &str, label: &str) -> Result<Address, GetNftInfoError> {
    let trimmed = text.trim();
    let bare = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    H160::from_str(bare).map_err(|err| GetNftInfoError::InvalidRequest(format!("invalid {label} '{text}': {err}")))
}

/// Encode the calldata for an ERC-721 `transferFrom` call.
pub(crate) fn encode_erc721_transfer_from(
    from: Address,
    to: Address,
    token_id: U256,
) -> Result<Vec<u8>, GetNftInfoError> {
    ERC721_CONTRACT
        .function("transferFrom")
        .map_err(|err| GetNftInfoError::Internal(format!("ERC-721 ABI lookup: {err}")))?
        .encode_input(&[Token::Address(from), Token::Address(to), Token::Uint(token_id)])
        .map_err(|err| GetNftInfoError::Internal(format!("ERC-721 encode: {err}")))
}

/// Encode the calldata for an ERC-1155 `safeTransferFrom` call with an
/// empty `data` payload.
pub(crate) fn encode_erc1155_safe_transfer_from(
    from: Address,
    to: Address,
    token_id: U256,
    amount: U256,
) -> Result<Vec<u8>, GetNftInfoError> {
    ERC1155_CONTRACT
        .function("safeTransferFrom")
        .map_err(|err| GetNftInfoError::Internal(format!("ERC-1155 ABI lookup: {err}")))?
        .encode_input(&[
            Token::Address(from),
            Token::Address(to),
            Token::Uint(token_id),
            Token::Uint(amount),
            Token::Bytes(Vec::new()),
        ])
        .map_err(|err| GetNftInfoError::Internal(format!("ERC-1155 encode: {err}")))
}

/// Encode a `balanceOf(address account, uint256 id)` call (ERC-1155).
fn encode_erc1155_balance_of(owner: Address, token_id: U256) -> Result<Vec<u8>, GetNftInfoError> {
    ERC1155_CONTRACT
        .function("balanceOf")
        .map_err(|err| GetNftInfoError::Internal(format!("ERC-1155 ABI lookup: {err}")))?
        .encode_input(&[Token::Address(owner), Token::Uint(token_id)])
        .map_err(|err| GetNftInfoError::Internal(format!("ERC-1155 encode: {err}")))
}

/// Resolve the [`EthCoin`] backing the platform layer of a chain.
async fn resolve_eth_coin(ctx: &MmArc, chain_ticker: &str) -> Result<EthCoin, GetNftInfoError> {
    let coin = lp_coinfind(ctx, chain_ticker)
        .await
        .map_err(GetNftInfoError::Internal)?
        .ok_or_else(|| GetNftInfoError::Internal(format!("Coin {chain_ticker} is not enabled")))?;
    match coin {
        MmCoinEnum::EthCoin(eth) => Ok(eth),
        _ => Err(GetNftInfoError::InvalidRequest(format!(
            "Coin {chain_ticker} is not an EVM coin"
        ))),
    }
}

/// Inputs shared by both ERC-721 and ERC-1155 transfer paths after the
/// per-standard calldata has been encoded.
struct EvmTxRequest {
    coin: EthCoin,
    contract_address: Address,
    to_address: Address,
    data: Vec<u8>,
    fee: Option<WithdrawFee>,
}

/// Build, sign and return (without broadcasting) the EVM transaction
/// that performs the NFT transfer described by `req`.
async fn build_signed_tx(ctx: &MmArc, req: EvmTxRequest) -> Result<TransactionDetails, GetNftInfoError> {
    let EvmTxRequest {
        coin,
        contract_address,
        to_address,
        data,
        fee,
    } = req;

    // ── gas / gas_price ────────────────────────────────────────────────
    let (gas, gas_price) = match fee {
        Some(WithdrawFee::EthGas { gas_price, gas }) => {
            let gp = wei_from_big_decimal(&gas_price, 9)
                .map_err(|err| GetNftInfoError::InvalidRequest(format!("gas_price: {err}")))?;
            (U256::from(gas), gp)
        },
        Some(other) => {
            return Err(GetNftInfoError::InvalidRequest(format!(
                "Expected 'EthGas' fee type, got {other:?}"
            )));
        },
        None => {
            let gp = coin
                .get_gas_price()
                .compat()
                .await
                .map_err(|err| GetNftInfoError::Transport(err.to_string()))?;
            let estimate_req = CallRequest {
                value: Some(U256::zero()),
                data: Some(data.clone().into()),
                from: Some(coin.my_address),
                to: contract_address,
                gas: None,
                gas_price: Some(gp),
            };
            let gas_limit = coin
                .estimate_gas(estimate_req)
                .compat()
                .await
                .map_err(|err| GetNftInfoError::Transport(format!("estimate_gas: {err}")))?;
            (gas_limit, gp)
        },
    };

    // ── nonce ──────────────────────────────────────────────────────────
    let _nonce_lock = NONCE_LOCK
        .lock(|_start, _now| -> Result<f64, GetNftInfoError> {
            if ctx.is_stopping() {
                return Err(GetNftInfoError::Internal(
                    "MM is stopping, aborting nft withdraw in NONCE_LOCK".to_owned(),
                ));
            }
            Ok(0.5)
        })
        .await?;

    let nonce_fut = get_addr_nonce(coin.my_address, coin.web3_instances.clone()).compat();
    let nonce = match select(nonce_fut, Timer::sleep(30.)).await {
        Either::Left((nonce_res, _)) => nonce_res.map_err(GetNftInfoError::Transport)?,
        Either::Right(_) => return Err(GetNftInfoError::Transport("Get address nonce timed out".to_owned())),
    };

    // ── sign ───────────────────────────────────────────────────────────
    let tx = UnSignedEthTx {
        nonce,
        value: U256::zero(),
        action: Action::Call(contract_address),
        data,
        gas,
        gas_price,
    };
    let signed = coin
        .sign_tx_for_send(tx)
        .map_err(|e| GetNftInfoError::Internal(e.to_string()))?;
    let bytes = crate::eth::rlp::encode(&signed);

    // ── fee details (in platform-coin units, e.g. ETH/MATIC/BNB) ──────
    let fee_coin = match &coin.coin_type {
        EthCoinType::Eth => coin.ticker.as_str().to_owned(),
        EthCoinType::Erc20 { platform, .. } => platform.clone(),
        EthCoinType::Tron | EthCoinType::Trc20 { .. } => {
            return Err(GetNftInfoError::InvalidRequest(
                "TRON family chains do not implement EVM NFT withdraws".to_owned(),
            ));
        },
    };
    let fee_details = EthTxFeeDetails::new(gas, gas_price, fee_coin.as_str())
        .map_err(|err| GetNftInfoError::Internal(format!("fee details: {err}")))?;
    let total_fee_dec = u256_to_big_decimal(gas * gas_price, coin.decimals)
        .map_err(|err| GetNftInfoError::Internal(err.to_string()))?;
    debug_assert_eq!(total_fee_dec, fee_details.total_fee);

    let my_address = coin.my_address().map_err(GetNftInfoError::Internal)?;

    Ok(TransactionDetails {
        to: vec![checksum_address(&format!("{:#02x}", to_address))],
        from: vec![my_address],
        total_amount: BigDecimal::from(0),
        my_balance_change: -fee_details.total_fee.clone(),
        spent_by_me: fee_details.total_fee.clone(),
        received_by_me: BigDecimal::from(0),
        tx_hex: bytes.into(),
        tx_hash: format!("{:02x}", signed.tx_hash()),
        block_height: 0,
        fee_details: Some(fee_details.into()),
        coin: coin.ticker.clone(),
        internal_id: BytesJson(Vec::new()),
        timestamp: now_ms() / 1000,
        kmd_rewards: None,
        transaction_type: Default::default(),
    })
}

/// Public entrypoint: builds the appropriate ERC-721 / ERC-1155 transfer
/// calldata, asks the EVM coin to sign it and returns the resulting
/// `TransactionDetails`.
pub async fn withdraw_nft(ctx: MmArc, req: WithdrawNftReq) -> MmResult<TransactionDetails, GetNftInfoError> {
    let result = match req {
        WithdrawNftReq::WithdrawErc721(inner) => withdraw_erc721(ctx, inner).await,
        WithdrawNftReq::WithdrawErc1155(inner) => withdraw_erc1155(ctx, inner).await,
    };
    result.map_err(MmError::new)
}

async fn withdraw_erc721(ctx: MmArc, req: WithdrawErc721) -> Result<TransactionDetails, GetNftInfoError> {
    let coin = resolve_eth_coin(&ctx, req.chain.coin_ticker()).await?;
    let contract = parse_eth_address(&req.token_address, "token_address")?;
    let to_addr = parse_eth_address(&req.to, "to")?;
    let token_id = biguint_to_u256(&req.token_id, "token_id")?;
    let data = encode_erc721_transfer_from(coin.my_address, to_addr, token_id)?;
    build_signed_tx(&ctx, EvmTxRequest {
        coin,
        contract_address: contract,
        to_address: to_addr,
        data,
        fee: req.fee,
    })
    .await
}

async fn withdraw_erc1155(ctx: MmArc, req: WithdrawErc1155) -> Result<TransactionDetails, GetNftInfoError> {
    let coin = resolve_eth_coin(&ctx, req.chain.coin_ticker()).await?;
    let contract = parse_eth_address(&req.token_address, "token_address")?;
    let to_addr = parse_eth_address(&req.to, "to")?;
    let token_id = biguint_to_u256(&req.token_id, "token_id")?;

    let amount = if req.max {
        let call_data = encode_erc1155_balance_of(coin.my_address, token_id)?;
        let call_req = CallRequest {
            value: Some(U256::zero()),
            data: Some(call_data.into()),
            from: Some(coin.my_address),
            to: contract,
            gas: None,
            gas_price: None,
        };
        // LP-17: alloy raw eth_call.
        use alloy::providers::Provider as _;
        let raw: crate::eth::Bytes = coin
            .web3
            .client()
            .request::<_, crate::eth::Bytes>("eth_call", (call_req, crate::eth::BlockNumber::Latest))
            .await
            .map_err(|err| GetNftInfoError::Transport(format!("balanceOf: {err}")))?;
        if raw.0.len() < 32 {
            return Err(GetNftInfoError::InvalidResponse(
                "balanceOf returned fewer than 32 bytes".to_owned(),
            ));
        }
        U256::from_big_endian(&raw.0[..32])
    } else {
        let requested = req
            .amount
            .as_ref()
            .map(|a| biguint_to_u256(a, "amount"))
            .transpose()?
            .unwrap_or_else(|| U256::from(1));
        if requested.is_zero() {
            return Err(GetNftInfoError::InvalidRequest("amount must be > 0".to_owned()));
        }
        requested
    };

    let data = encode_erc1155_safe_transfer_from(coin.my_address, to_addr, token_id, amount)?;
    build_signed_tx(&ctx, EvmTxRequest {
        coin,
        contract_address: contract,
        to_address: to_addr,
        data,
        fee: req.fee,
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(byte: u8) -> Address {
        let mut bytes = [0u8; 20];
        bytes[19] = byte;
        Address::from(bytes)
    }

    /// `transferFrom(from,to,id)` selector is `0x23b872dd`.
    #[test]
    fn erc721_calldata_starts_with_transfer_from_selector() {
        let data = encode_erc721_transfer_from(addr(1), addr(2), U256::from(42u32)).unwrap();
        assert_eq!(&data[..4], &[0x23, 0xb8, 0x72, 0xdd]);
        assert_eq!(data.len(), 4 + 32 * 3);
        assert_eq!(data[data.len() - 1], 42);
    }

    /// `safeTransferFrom(from,to,id,value,bytes)` selector is `0xf242432a`.
    #[test]
    fn erc1155_calldata_starts_with_safe_transfer_from_selector() {
        let data = encode_erc1155_safe_transfer_from(addr(1), addr(2), U256::from(7u32), U256::from(3u32)).unwrap();
        assert_eq!(&data[..4], &[0xf2, 0x42, 0x43, 0x2a]);
        // 4 selector + 5 head words (32 each) + 1 length word for empty bytes
        assert_eq!(data.len(), 4 + 32 * 5 + 32);
    }

    #[test]
    fn parse_eth_address_accepts_with_and_without_prefix() {
        let with = parse_eth_address("0x000000000000000000000000000000000000abcd", "addr").unwrap();
        let without = parse_eth_address("000000000000000000000000000000000000abcd", "addr").unwrap();
        assert_eq!(with, without);
    }

    #[test]
    fn parse_eth_address_rejects_garbage() {
        let err = parse_eth_address("not-an-address", "addr").unwrap_err();
        match err {
            GetNftInfoError::InvalidRequest(_) => (),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn biguint_to_u256_accepts_max_uint256() {
        let max = BigUint::from(2u32).pow(256u32) - BigUint::from(1u32);
        biguint_to_u256(&max, "id").expect("max uint256 fits");
    }

    #[test]
    fn biguint_to_u256_rejects_over_uint256() {
        let too_big = BigUint::from(2u32).pow(256u32);
        let err = biguint_to_u256(&too_big, "id").unwrap_err();
        match err {
            GetNftInfoError::InvalidRequest(_) => (),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
