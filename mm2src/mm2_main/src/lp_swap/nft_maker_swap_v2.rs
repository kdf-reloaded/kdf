//! NFT maker swap V2 driver (P10.3.7.d).
//!
//! Thin state-machine wiring on top of [`coins::eth::EthCoin`]'s
//! NFT-aware build helpers (P10.3.7.c). This module is the bridge
//! between an order's negotiated [`SwapVersion`] and the NFT HTLC
//! call execution: it decides whether a pair runs the NFT V2
//! protocol, and broadcasts the resulting [`NftCall`] via the EVM
//! `sign_and_send_transaction` path.
//!
//! The typed dispatch and restart decisions below are the boundary that must be
//! used before a maker-NFT candidate can enter the generic V2 state machines.
//! If the caller cannot recover the maker NFT identity from corpus-compatible
//! state, restart must park instead of falling through to fungible maker ops.

use crate::mm2::lp_swap::swap_versioning::SwapVersion;
use coins::eth::legacy_tx::Action;
use coins::eth::nft_swap_v2::{decode_nft_maker_calldata, DecodedNftMakerCalldata, DecodedNftMakerPayment,
                              EthCoinNftError, NftKind, NftMakerCalldataKind, NftMakerPaymentArgs,
                              NftRefundSecretArgs, NftRefundTimelockArgs, NftSpendMakerPaymentArgs, NftSwapV2Error};
use coins::eth::{EthCoin, SignedEthTx};
use ethereum_types::{Address, U256};
use futures::compat::Future01CompatExt;

/// Outcome of a `should_use_nft_swap_v2` check.
///
/// Carries enough information for callers to log the negotiation
/// decision and surface meaningful diagnostics on rejection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NftSwapV2NegotiationOutcome {
    /// Both sides advertised NFT V2 and the maker coin has an NFT
    /// HTLC contract configured. Run the NFT V2 path.
    Use,
    /// One or both sides did not advertise the NFT V2 protocol
    /// version. Fall back to the negotiated baseline (TPU or legacy).
    VersionMismatch { maker: SwapVersion, taker: SwapVersion },
    /// Both sides advertise NFT V2 but the maker coin has no NFT
    /// HTLC contract configured for this chain. Hard-fail the swap
    /// rather than silently running a non-NFT path.
    NoNftContract,
}

/// Decide whether a maker/taker pair should execute the NFT swap V2
/// protocol. Returns [`NftSwapV2NegotiationOutcome::Use`] only when
/// **both sides** advertise [`NFT_SWAP_V2_VERSION`] AND the maker
/// coin reports a configured NFT HTLC contract.
///
/// Pure function — does not touch network state.
pub fn should_use_nft_swap_v2(
    maker_version: SwapVersion,
    taker_version: SwapVersion,
    maker_has_nft_contract: bool,
) -> NftSwapV2NegotiationOutcome {
    if !maker_version.is_nft_v2() || !taker_version.is_nft_v2() {
        return NftSwapV2NegotiationOutcome::VersionMismatch {
            maker: maker_version,
            taker: taker_version,
        };
    }
    if !maker_has_nft_contract {
        return NftSwapV2NegotiationOutcome::NoNftContract;
    }
    NftSwapV2NegotiationOutcome::Use
}

/// A maker-side NFT that is eligible for the NFT swap V2 maker branch.
///
/// The constructors encode the ERC-721 vs ERC-1155 shape required by Chapter
/// 17: ERC-721 carries exactly one token id, while ERC-1155 carries token id
/// plus a non-zero amount. Activation code is still responsible for proving
/// the token belongs to an enabled EVM NFT collection before constructing this
/// value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvmNftMakerAsset {
    kind: NftKind,
    token_address: Address,
    token_id: U256,
    amount: Option<U256>,
}

impl EvmNftMakerAsset {
    pub fn erc721(token_address: Address, token_id: U256) -> Self {
        EvmNftMakerAsset {
            kind: NftKind::Erc721,
            token_address,
            token_id,
            amount: None,
        }
    }

    pub fn erc1155(token_address: Address, token_id: U256, amount: U256) -> Result<Self, EvmNftMakerAssetError> {
        if amount.is_zero() {
            return Err(EvmNftMakerAssetError::ZeroErc1155Amount);
        }

        Ok(EvmNftMakerAsset {
            kind: NftKind::Erc1155,
            token_address,
            token_id,
            amount: Some(amount),
        })
    }

    pub fn kind(&self) -> NftKind { self.kind }

    pub fn token_address(&self) -> Address { self.token_address }

    pub fn token_id(&self) -> U256 { self.token_id }

    pub fn amount(&self) -> Option<U256> { self.amount }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvmNftMakerAssetError {
    ZeroErc1155Amount,
}

impl std::fmt::Display for EvmNftMakerAssetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EvmNftMakerAssetError::ZeroErc1155Amount => write!(f, "ERC-1155 NFT swap amount must be non-zero"),
        }
    }
}

impl std::error::Error for EvmNftMakerAssetError {}

/// Taker payment asset family for the NFT V2 dispatcher.
///
/// NFT V2 is maker-NFT-for-taker-fungible only; there is intentionally no
/// taker-side NFT operation surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NftSwapV2TakerAsset {
    Fungible,
    Nft,
}

/// Maker-side state-machine actions that must be routed through the NFT
/// maker-operation surface for a selected maker-NFT V2 candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NftMakerPaymentOperation {
    Send,
    Validate,
    TakerSpend,
    RefundTimelock,
    RefundSecret,
}

pub const NFT_MAKER_PAYMENT_OPERATIONS: [NftMakerPaymentOperation; 5] = [
    NftMakerPaymentOperation::Send,
    NftMakerPaymentOperation::Validate,
    NftMakerPaymentOperation::TakerSpend,
    NftMakerPaymentOperation::RefundTimelock,
    NftMakerPaymentOperation::RefundSecret,
];

/// Production binding between a maker-NFT candidate and the NFT maker-payment
/// operation surface. This is intentionally separate from the ordinary
/// fungible [`MakerCoinSwapOpsV2`](coins::MakerCoinSwapOpsV2) branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NftMakerStateMachineBinding {
    pub kind: NftKind,
    pub token_address: Address,
    pub token_id: U256,
    pub amount: Option<U256>,
    pub operations: [NftMakerPaymentOperation; 5],
}

impl NftMakerStateMachineBinding {
    pub fn from_asset(asset: &EvmNftMakerAsset) -> Self {
        NftMakerStateMachineBinding {
            kind: asset.kind(),
            token_address: asset.token_address(),
            token_id: asset.token_id(),
            amount: asset.amount(),
            operations: NFT_MAKER_PAYMENT_OPERATIONS,
        }
    }

    pub fn from_decoded_payment(decoded: &DecodedNftMakerPayment) -> Self {
        NftMakerStateMachineBinding {
            kind: decoded.kind,
            token_address: decoded.token_address,
            token_id: decoded.token_id,
            amount: decoded.amount,
            operations: NFT_MAKER_PAYMENT_OPERATIONS,
        }
    }

    pub fn uses_nft_operation(&self, operation: NftMakerPaymentOperation) -> bool {
        self.operations.contains(&operation)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NftSwapV2MakerBranch {
    Nft { binding: NftMakerStateMachineBinding },
    NegotiatedFungiblePath,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NftSwapV2TakerBranch {
    FungibleEvmV2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NftSwapV2Fallback {
    NegotiatedFungiblePathIfRepresentable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NftSwapV2Refusal {
    MakerNftVersionMismatch { maker: SwapVersion, taker: SwapVersion },
    NoNftContractConfigured,
    UnsupportedTakerNft,
}

/// State-machine dispatch result for a candidate maker-NFT V2 swap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NftSwapV2StateMachineDispatch {
    UseNftV2Path {
        maker: NftSwapV2MakerBranch,
        taker: NftSwapV2TakerBranch,
    },
    VersionMismatch {
        maker: SwapVersion,
        taker: SwapVersion,
        fallback: NftSwapV2Fallback,
    },
    VersionMismatchRefused {
        maker: SwapVersion,
        taker: SwapVersion,
        reason: NftSwapV2Refusal,
    },
    NoNftContractConfigured,
    UnsupportedTakerNft,
}

/// Resolve the maker/taker state-machine branches for a candidate maker-NFT
/// swap. This is a pure local decision: no network or chain state is read.
pub fn select_nft_swap_v2_state_machine_dispatch(
    maker_version: SwapVersion,
    taker_version: SwapVersion,
    maker_has_nft_contract: bool,
    maker_asset: &EvmNftMakerAsset,
    taker_asset: NftSwapV2TakerAsset,
) -> NftSwapV2StateMachineDispatch {
    select_nft_swap_v2_state_machine_dispatch_inner(
        maker_version,
        taker_version,
        maker_has_nft_contract,
        maker_asset,
        taker_asset,
        false,
    )
}

/// Resolve maker/taker branches when the caller has proven the maker asset can
/// also be represented by the negotiated fungible path. Maker-NFT candidates
/// must use [`select_nft_swap_v2_state_machine_dispatch`], which refuses
/// version-mismatched NFTs instead of falling back.
pub fn select_fungible_compatible_nft_swap_v2_state_machine_dispatch(
    maker_version: SwapVersion,
    taker_version: SwapVersion,
    maker_has_nft_contract: bool,
    maker_asset: &EvmNftMakerAsset,
    taker_asset: NftSwapV2TakerAsset,
) -> NftSwapV2StateMachineDispatch {
    select_nft_swap_v2_state_machine_dispatch_inner(
        maker_version,
        taker_version,
        maker_has_nft_contract,
        maker_asset,
        taker_asset,
        true,
    )
}

fn select_nft_swap_v2_state_machine_dispatch_inner(
    maker_version: SwapVersion,
    taker_version: SwapVersion,
    maker_has_nft_contract: bool,
    maker_asset: &EvmNftMakerAsset,
    taker_asset: NftSwapV2TakerAsset,
    maker_asset_fungible_compatible: bool,
) -> NftSwapV2StateMachineDispatch {
    if taker_asset == NftSwapV2TakerAsset::Nft {
        return NftSwapV2StateMachineDispatch::UnsupportedTakerNft;
    }

    match should_use_nft_swap_v2(maker_version, taker_version, maker_has_nft_contract) {
        NftSwapV2NegotiationOutcome::Use => NftSwapV2StateMachineDispatch::UseNftV2Path {
            maker: NftSwapV2MakerBranch::Nft {
                binding: NftMakerStateMachineBinding::from_asset(maker_asset),
            },
            taker: NftSwapV2TakerBranch::FungibleEvmV2,
        },
        NftSwapV2NegotiationOutcome::VersionMismatch { maker, taker } if maker_asset_fungible_compatible => {
            NftSwapV2StateMachineDispatch::VersionMismatch {
                maker,
                taker,
                fallback: NftSwapV2Fallback::NegotiatedFungiblePathIfRepresentable,
            }
        },
        NftSwapV2NegotiationOutcome::VersionMismatch { maker, taker } => {
            NftSwapV2StateMachineDispatch::VersionMismatchRefused {
                maker,
                taker,
                reason: NftSwapV2Refusal::MakerNftVersionMismatch { maker, taker },
            }
        },
        NftSwapV2NegotiationOutcome::NoNftContract => NftSwapV2StateMachineDispatch::NoNftContractConfigured,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NftSwapV2RestartParkReason {
    PreMakerPaymentNftIdentityUnavailable,
    TokenStandardUnavailable,
    MakerPaymentCalldataMalformed(String),
    MakerPaymentCalldataInconsistent(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
// This is a swap state-machine decision enum; boxing the large variant would add
// heap indirection per transition for no real memory benefit here.
#[allow(clippy::large_enum_variant)]
pub enum NftSwapV2RestartDecision {
    UseNftV2Path {
        maker: NftMakerStateMachineBinding,
        taker: NftSwapV2TakerBranch,
        decoded_maker_payment: DecodedNftMakerPayment,
    },
    Park(NftSwapV2RestartParkReason),
    Refuse(NftSwapV2Refusal),
}

pub struct NftSwapV2PostMakerPaymentRestart<'a> {
    pub maker_version: SwapVersion,
    pub taker_version: SwapVersion,
    pub configured_nft_contract: Option<Address>,
    pub taker_asset: NftSwapV2TakerAsset,
    pub token_standard: Option<NftKind>,
    pub tx_to: Address,
    pub calldata: &'a [u8],
    pub expected: Option<&'a NftMakerPaymentArgs>,
}

/// Pre-maker-payment restart has no compatible native DB source for NFT
/// identity. The caller must park/refuse instead of fabricating calldata.
pub fn select_nft_swap_v2_pre_maker_payment_restart() -> NftSwapV2RestartDecision {
    NftSwapV2RestartDecision::Park(NftSwapV2RestartParkReason::PreMakerPaymentNftIdentityUnavailable)
}

/// Decode and validate persisted maker-payment calldata before restoring a
/// maker-NFT state-machine branch. This is the production restart boundary:
/// without a configured contract, token standard, and valid calldata, recovery
/// parks/refuses rather than guessing.
pub fn select_nft_swap_v2_post_maker_payment_restart(
    input: NftSwapV2PostMakerPaymentRestart<'_>,
) -> NftSwapV2RestartDecision {
    if input.taker_asset == NftSwapV2TakerAsset::Nft {
        return NftSwapV2RestartDecision::Refuse(NftSwapV2Refusal::UnsupportedTakerNft);
    }

    let Some(contract) = input.configured_nft_contract else {
        return NftSwapV2RestartDecision::Refuse(NftSwapV2Refusal::NoNftContractConfigured);
    };

    match should_use_nft_swap_v2(input.maker_version, input.taker_version, true) {
        NftSwapV2NegotiationOutcome::Use => (),
        NftSwapV2NegotiationOutcome::VersionMismatch { maker, taker } => {
            return NftSwapV2RestartDecision::Refuse(NftSwapV2Refusal::MakerNftVersionMismatch { maker, taker })
        },
        NftSwapV2NegotiationOutcome::NoNftContract => {
            return NftSwapV2RestartDecision::Refuse(NftSwapV2Refusal::NoNftContractConfigured)
        },
    }

    let Some(kind) = input.token_standard else {
        return NftSwapV2RestartDecision::Park(NftSwapV2RestartParkReason::TokenStandardUnavailable);
    };

    if input.tx_to != contract {
        return NftSwapV2RestartDecision::Park(NftSwapV2RestartParkReason::MakerPaymentCalldataInconsistent(format!(
            "expected NFT maker contract {contract:?}, got {:?}",
            input.tx_to
        )));
    }

    let decoded = match decode_nft_maker_calldata(kind, NftMakerCalldataKind::MakerPayment, input.calldata) {
        Ok(DecodedNftMakerCalldata::MakerPayment(decoded)) => decoded,
        Ok(DecodedNftMakerCalldata::SpendMakerPayment(_)) => {
            return NftSwapV2RestartDecision::Park(NftSwapV2RestartParkReason::MakerPaymentCalldataMalformed(
                "decoded spend calldata while restoring maker payment".to_owned(),
            ))
        },
        Err(e) => {
            return NftSwapV2RestartDecision::Park(NftSwapV2RestartParkReason::MakerPaymentCalldataMalformed(
                e.to_string(),
            ))
        },
    };

    if let Some(expected) = input.expected {
        if let Err(e) = validate_restored_maker_payment(&decoded, expected) {
            return NftSwapV2RestartDecision::Park(NftSwapV2RestartParkReason::MakerPaymentCalldataInconsistent(
                e.to_string(),
            ));
        }
    }

    NftSwapV2RestartDecision::UseNftV2Path {
        maker: NftMakerStateMachineBinding::from_decoded_payment(&decoded),
        taker: NftSwapV2TakerBranch::FungibleEvmV2,
        decoded_maker_payment: decoded,
    }
}

pub fn select_nft_swap_v2_post_maker_payment_restart_from_tx<'a>(
    mut input: NftSwapV2PostMakerPaymentRestart<'a>,
    tx: &'a SignedEthTx,
) -> NftSwapV2RestartDecision {
    let tx_to = match tx.transaction.action {
        Action::Call(to) => to,
        Action::Create => {
            return NftSwapV2RestartDecision::Park(NftSwapV2RestartParkReason::MakerPaymentCalldataMalformed(
                "maker payment transaction creates a contract".to_owned(),
            ))
        },
    };
    input.tx_to = tx_to;
    input.calldata = &tx.transaction.data;
    select_nft_swap_v2_post_maker_payment_restart(input)
}

fn validate_restored_maker_payment(
    decoded: &DecodedNftMakerPayment,
    expected: &NftMakerPaymentArgs,
) -> Result<(), NftSwapV2Error> {
    if decoded.kind != expected.kind {
        return Err(NftSwapV2Error::Mismatch {
            field: "kind",
            detail: format!("expected {:?}, got {:?}", expected.kind, decoded.kind),
        });
    }
    if decoded.swap_id != expected.swap_id {
        return Err(NftSwapV2Error::Mismatch {
            field: "id",
            detail: "decoded swap id does not match negotiation".to_owned(),
        });
    }
    if decoded.amount != expected.amount {
        return Err(NftSwapV2Error::Mismatch {
            field: "amount",
            detail: format!("expected {:?}, got {:?}", expected.amount, decoded.amount),
        });
    }
    if decoded.taker != expected.taker {
        return Err(NftSwapV2Error::Mismatch {
            field: "taker",
            detail: format!("expected {:?}, got {:?}", expected.taker, decoded.taker),
        });
    }
    if decoded.taker_secret_hash != expected.taker_secret_hash {
        return Err(NftSwapV2Error::Mismatch {
            field: "takerSecretHash",
            detail: "decoded taker secret hash does not match negotiation".to_owned(),
        });
    }
    if decoded.maker_secret_hash != expected.maker_secret_hash {
        return Err(NftSwapV2Error::Mismatch {
            field: "makerSecretHash",
            detail: "decoded maker secret hash does not match negotiation".to_owned(),
        });
    }
    if decoded.payment_time_lock != expected.payment_time_lock {
        return Err(NftSwapV2Error::Mismatch {
            field: "paymentLockTime",
            detail: format!(
                "expected {}, got {}",
                expected.payment_time_lock, decoded.payment_time_lock
            ),
        });
    }
    if decoded.token_address != expected.token_address {
        return Err(NftSwapV2Error::Mismatch {
            field: "tokenAddress",
            detail: format!("expected {:?}, got {:?}", expected.token_address, decoded.token_address),
        });
    }
    if decoded.token_id != expected.token_id {
        return Err(NftSwapV2Error::Mismatch {
            field: "tokenId",
            detail: format!("expected {}, got {}", expected.token_id, decoded.token_id),
        });
    }
    Ok(())
}

/// Errors raised by the broadcasting drivers.
#[derive(Debug)]
pub enum NftSwapV2DriverError {
    /// The build phase rejected the inputs (e.g. ERC-1155 without amount).
    Build(EthCoinNftError),
    /// Broadcasting the signed transaction failed.
    Broadcast(String),
}

impl std::fmt::Display for NftSwapV2DriverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NftSwapV2DriverError::Build(e) => write!(f, "NFT swap V2 build error: {e}"),
            NftSwapV2DriverError::Broadcast(e) => write!(f, "NFT swap V2 broadcast error: {e}"),
        }
    }
}

impl std::error::Error for NftSwapV2DriverError {}

impl From<EthCoinNftError> for NftSwapV2DriverError {
    fn from(e: EthCoinNftError) -> Self { NftSwapV2DriverError::Build(e) }
}

async fn broadcast(
    coin: &EthCoin,
    call: coins::eth::nft_swap_v2::NftCall,
) -> Result<SignedEthTx, NftSwapV2DriverError> {
    coin.send_nft_call(call)
        .compat()
        .await
        .map_err(|e| NftSwapV2DriverError::Broadcast(format!("{e:?}")))
}

/// Build and broadcast an `erc{721,1155}MakerPayment` HTLC.
pub async fn dispatch_nft_maker_payment(
    coin: &EthCoin,
    args: &NftMakerPaymentArgs,
) -> Result<SignedEthTx, NftSwapV2DriverError> {
    let call = coin.build_send_nft_maker_payment(args)?;
    broadcast(coin, call).await
}

/// Build and broadcast a `spendErc{721,1155}MakerPayment` (taker reveals
/// `maker_secret` to claim the NFT).
pub async fn dispatch_nft_spend_maker_payment(
    coin: &EthCoin,
    args: &NftSpendMakerPaymentArgs,
) -> Result<SignedEthTx, NftSwapV2DriverError> {
    let call = coin.build_spend_nft_maker_payment(args)?;
    broadcast(coin, call).await
}

/// Build and broadcast a maker timelock refund of an NFT HTLC.
pub async fn dispatch_nft_refund_maker_payment_timelock(
    coin: &EthCoin,
    args: &NftRefundTimelockArgs,
) -> Result<SignedEthTx, NftSwapV2DriverError> {
    let call = coin.build_refund_nft_maker_payment_timelock(args)?;
    broadcast(coin, call).await
}

/// Build and broadcast a cooperative-secret refund of an NFT HTLC.
pub async fn dispatch_nft_refund_maker_payment_secret(
    coin: &EthCoin,
    args: &NftRefundSecretArgs,
) -> Result<SignedEthTx, NftSwapV2DriverError> {
    let call = coin.build_refund_nft_maker_payment_secret(args)?;
    broadcast(coin, call).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mm2::lp_swap::swap_versioning::{LEGACY_SWAP_VERSION, NFT_SWAP_V2_VERSION, TPU_SWAP_VERSION};
    use coins::eth::nft_swap_v2::encode_maker_payment;

    fn v(n: u8) -> SwapVersion { SwapVersion { version: n } }

    fn token_address() -> Address {
        let mut bytes = [0u8; 20];
        bytes[18] = 0xE7;
        bytes[19] = 0x21;
        Address::from(bytes)
    }

    fn erc721_asset() -> EvmNftMakerAsset { EvmNftMakerAsset::erc721(token_address(), U256::from(721u64)) }

    fn erc1155_asset() -> EvmNftMakerAsset {
        EvmNftMakerAsset::erc1155(token_address(), U256::from(1155u64), U256::from(3u64)).unwrap()
    }

    fn contract_address() -> Address {
        let mut bytes = [0u8; 20];
        bytes[19] = 0x17;
        Address::from(bytes)
    }

    fn expected_args(kind: NftKind) -> NftMakerPaymentArgs {
        let asset = match kind {
            NftKind::Erc721 => erc721_asset(),
            NftKind::Erc1155 => erc1155_asset(),
        };
        NftMakerPaymentArgs {
            kind,
            swap_id: [0x11; 32],
            amount: asset.amount(),
            taker: contract_address(),
            taker_secret_hash: [0x22; 32],
            maker_secret_hash: [0x33; 32],
            payment_time_lock: 1_700_000_000,
            token_address: asset.token_address(),
            token_id: asset.token_id(),
        }
    }

    #[test]
    fn should_use_when_both_sides_advertise_nft_v2_and_contract_present() {
        assert_eq!(
            should_use_nft_swap_v2(v(NFT_SWAP_V2_VERSION), v(NFT_SWAP_V2_VERSION), true),
            NftSwapV2NegotiationOutcome::Use
        );
    }

    #[test]
    fn rejects_when_maker_only_advertises_tpu() {
        let out = should_use_nft_swap_v2(v(TPU_SWAP_VERSION), v(NFT_SWAP_V2_VERSION), true);
        assert!(matches!(out, NftSwapV2NegotiationOutcome::VersionMismatch { .. }));
    }

    #[test]
    fn rejects_when_taker_only_advertises_tpu() {
        let out = should_use_nft_swap_v2(v(NFT_SWAP_V2_VERSION), v(TPU_SWAP_VERSION), true);
        assert!(matches!(out, NftSwapV2NegotiationOutcome::VersionMismatch { .. }));
    }

    #[test]
    fn rejects_when_either_side_is_legacy() {
        for (m, t) in [
            (LEGACY_SWAP_VERSION, NFT_SWAP_V2_VERSION),
            (NFT_SWAP_V2_VERSION, LEGACY_SWAP_VERSION),
            (LEGACY_SWAP_VERSION, LEGACY_SWAP_VERSION),
        ] {
            let out = should_use_nft_swap_v2(v(m), v(t), true);
            assert!(matches!(out, NftSwapV2NegotiationOutcome::VersionMismatch { .. }));
        }
    }

    #[test]
    fn rejects_when_maker_has_no_nft_contract() {
        assert_eq!(
            should_use_nft_swap_v2(v(NFT_SWAP_V2_VERSION), v(NFT_SWAP_V2_VERSION), false),
            NftSwapV2NegotiationOutcome::NoNftContract
        );
    }

    #[test]
    fn version_mismatch_carries_advertised_versions() {
        let out = should_use_nft_swap_v2(v(LEGACY_SWAP_VERSION), v(NFT_SWAP_V2_VERSION), true);
        match out {
            NftSwapV2NegotiationOutcome::VersionMismatch { maker, taker } => {
                assert_eq!(maker.version, LEGACY_SWAP_VERSION);
                assert_eq!(taker.version, NFT_SWAP_V2_VERSION);
            },
            _ => panic!("expected VersionMismatch"),
        }
    }

    #[test]
    fn t17_9_1_use_nft_v2_path_dispatches_erc721_and_erc1155_maker_with_fungible_taker() {
        for asset in [erc721_asset(), erc1155_asset()] {
            let dispatch = select_nft_swap_v2_state_machine_dispatch(
                v(NFT_SWAP_V2_VERSION),
                v(NFT_SWAP_V2_VERSION),
                true,
                &asset,
                NftSwapV2TakerAsset::Fungible,
            );

            let binding = NftMakerStateMachineBinding::from_asset(&asset);
            assert_eq!(dispatch, NftSwapV2StateMachineDispatch::UseNftV2Path {
                maker: NftSwapV2MakerBranch::Nft { binding },
                taker: NftSwapV2TakerBranch::FungibleEvmV2,
            });
            for operation in NFT_MAKER_PAYMENT_OPERATIONS {
                assert!(binding.uses_nft_operation(operation));
            }
        }
    }

    #[test]
    fn t17_9_3_version_mismatch_refuses_maker_nft_before_fungible_fallback() {
        for (maker_version, taker_version) in [
            (NFT_SWAP_V2_VERSION, TPU_SWAP_VERSION),
            (TPU_SWAP_VERSION, NFT_SWAP_V2_VERSION),
        ] {
            let dispatch = select_nft_swap_v2_state_machine_dispatch(
                v(maker_version),
                v(taker_version),
                true,
                &erc721_asset(),
                NftSwapV2TakerAsset::Fungible,
            );

            assert_eq!(dispatch, NftSwapV2StateMachineDispatch::VersionMismatchRefused {
                maker: v(maker_version),
                taker: v(taker_version),
                reason: NftSwapV2Refusal::MakerNftVersionMismatch {
                    maker: v(maker_version),
                    taker: v(taker_version),
                },
            });
            assert!(!matches!(dispatch, NftSwapV2StateMachineDispatch::UseNftV2Path {
                maker: NftSwapV2MakerBranch::Nft { .. },
                ..
            }));
        }
    }

    #[test]
    fn t17_9_3_version_mismatch_keeps_fallback_only_when_fungible_compatible() {
        let dispatch = select_fungible_compatible_nft_swap_v2_state_machine_dispatch(
            v(NFT_SWAP_V2_VERSION),
            v(TPU_SWAP_VERSION),
            true,
            &erc721_asset(),
            NftSwapV2TakerAsset::Fungible,
        );

        assert_eq!(dispatch, NftSwapV2StateMachineDispatch::VersionMismatch {
            maker: v(NFT_SWAP_V2_VERSION),
            taker: v(TPU_SWAP_VERSION),
            fallback: NftSwapV2Fallback::NegotiatedFungiblePathIfRepresentable,
        });
    }

    #[test]
    fn t17_9_3_no_nft_contract_refuses_without_fallback() {
        let dispatch = select_nft_swap_v2_state_machine_dispatch(
            v(NFT_SWAP_V2_VERSION),
            v(NFT_SWAP_V2_VERSION),
            false,
            &erc721_asset(),
            NftSwapV2TakerAsset::Fungible,
        );

        assert_eq!(dispatch, NftSwapV2StateMachineDispatch::NoNftContractConfigured);
        assert!(!matches!(dispatch, NftSwapV2StateMachineDispatch::VersionMismatch {
            fallback: NftSwapV2Fallback::NegotiatedFungiblePathIfRepresentable,
            ..
        }));
    }

    #[test]
    fn t17_9_4_rejects_taker_nft_and_keeps_fungible_taker_for_valid_maker_nft() {
        let unsupported = select_nft_swap_v2_state_machine_dispatch(
            v(NFT_SWAP_V2_VERSION),
            v(NFT_SWAP_V2_VERSION),
            true,
            &erc721_asset(),
            NftSwapV2TakerAsset::Nft,
        );
        assert_eq!(unsupported, NftSwapV2StateMachineDispatch::UnsupportedTakerNft);

        let supported = select_nft_swap_v2_state_machine_dispatch(
            v(NFT_SWAP_V2_VERSION),
            v(NFT_SWAP_V2_VERSION),
            true,
            &erc1155_asset(),
            NftSwapV2TakerAsset::Fungible,
        );
        assert!(matches!(supported, NftSwapV2StateMachineDispatch::UseNftV2Path {
            maker: NftSwapV2MakerBranch::Nft {
                binding: NftMakerStateMachineBinding {
                    kind: NftKind::Erc1155,
                    ..
                },
            },
            taker: NftSwapV2TakerBranch::FungibleEvmV2,
        }));
    }

    #[test]
    fn r17_9_1_erc1155_maker_asset_requires_non_zero_amount() {
        assert_eq!(
            EvmNftMakerAsset::erc1155(token_address(), U256::from(1155u64), U256::zero()),
            Err(EvmNftMakerAssetError::ZeroErc1155Amount)
        );
    }

    #[test]
    fn t17_9_9_pre_maker_payment_restart_parks_without_fabricated_nft_identity() {
        assert_eq!(
            select_nft_swap_v2_pre_maker_payment_restart(),
            NftSwapV2RestartDecision::Park(NftSwapV2RestartParkReason::PreMakerPaymentNftIdentityUnavailable)
        );
    }

    #[test]
    fn t17_9_10_post_maker_payment_restart_decodes_calldata_and_routes_nft_binding() {
        let expected = expected_args(NftKind::Erc1155);
        let calldata = encode_maker_payment(&expected).unwrap();

        let decision = select_nft_swap_v2_post_maker_payment_restart(NftSwapV2PostMakerPaymentRestart {
            maker_version: v(NFT_SWAP_V2_VERSION),
            taker_version: v(NFT_SWAP_V2_VERSION),
            configured_nft_contract: Some(contract_address()),
            taker_asset: NftSwapV2TakerAsset::Fungible,
            token_standard: Some(NftKind::Erc1155),
            tx_to: contract_address(),
            calldata: &calldata,
            expected: Some(&expected),
        });

        match decision {
            NftSwapV2RestartDecision::UseNftV2Path {
                maker,
                taker,
                decoded_maker_payment,
            } => {
                assert_eq!(taker, NftSwapV2TakerBranch::FungibleEvmV2);
                assert_eq!(maker.kind, NftKind::Erc1155);
                assert_eq!(maker.token_address, expected.token_address);
                assert_eq!(maker.token_id, expected.token_id);
                assert_eq!(maker.amount, expected.amount);
                assert_eq!(decoded_maker_payment.token_address, expected.token_address);
                for operation in NFT_MAKER_PAYMENT_OPERATIONS {
                    assert!(maker.uses_nft_operation(operation));
                }
            },
            other => panic!("expected UseNftV2Path, got {other:?}"),
        }
    }

    #[test]
    fn t17_9_10_post_maker_payment_restart_parks_without_standard_or_on_inconsistent_calldata() {
        let expected = expected_args(NftKind::Erc721);
        let calldata = encode_maker_payment(&expected).unwrap();

        let no_standard = select_nft_swap_v2_post_maker_payment_restart(NftSwapV2PostMakerPaymentRestart {
            maker_version: v(NFT_SWAP_V2_VERSION),
            taker_version: v(NFT_SWAP_V2_VERSION),
            configured_nft_contract: Some(contract_address()),
            taker_asset: NftSwapV2TakerAsset::Fungible,
            token_standard: None,
            tx_to: contract_address(),
            calldata: &calldata,
            expected: Some(&expected),
        });
        assert_eq!(
            no_standard,
            NftSwapV2RestartDecision::Park(NftSwapV2RestartParkReason::TokenStandardUnavailable)
        );

        let mut wrong_expected = expected_args(NftKind::Erc721);
        wrong_expected.token_id = U256::from(999u64);
        let inconsistent = select_nft_swap_v2_post_maker_payment_restart(NftSwapV2PostMakerPaymentRestart {
            maker_version: v(NFT_SWAP_V2_VERSION),
            taker_version: v(NFT_SWAP_V2_VERSION),
            configured_nft_contract: Some(contract_address()),
            taker_asset: NftSwapV2TakerAsset::Fungible,
            token_standard: Some(NftKind::Erc721),
            tx_to: contract_address(),
            calldata: &calldata,
            expected: Some(&wrong_expected),
        });
        assert!(matches!(
            inconsistent,
            NftSwapV2RestartDecision::Park(NftSwapV2RestartParkReason::MakerPaymentCalldataInconsistent(_))
        ));
    }
}
