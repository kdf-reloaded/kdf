use crate::context::CoinsActivationContext;
use crate::l2::{EnableL2Error, InitL2ActivationOps, L2ActivationError, L2ActivationOps, L2InitialStatus,
                L2ProtocolParams, L2TaskManagerShared};
use crate::prelude::*;
use async_trait::async_trait;
use coins::lightning::ln_conf::{LightningCoinConf, LightningProtocolConf};
use coins::lightning::ln_errors::EnableLightningError;
use coins::lightning::{start_lightning, LightningCoin, LightningParams};
use coins::utxo::utxo_standard::UtxoStandardCoin;
use coins::utxo::UtxoCommonOps;
use coins::{coin_conf, BalanceError, CoinBalance, CoinProtocol, MarketCoinOps, MmCoinEnum, RegisterCoinError};
use common::{HttpStatusCode, StatusCode};
use derive_more::Display;
use futures::compat::Future01CompatExt;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use rpc_task::RpcTaskHandle;
use ser_error_derive::SerializeErrorType;
use serde_derive::{Deserialize, Serialize};
use serde_json::{self as json, Value as Json};

/// The node colour used when the request has none (CRD ch.41 R41.2.4).
const DEFAULT_NODE_COLOR: &str = "2b6680";

const DEFAULT_LISTENING_PORT: u16 = 9735;

impl TryPlatformCoinFromMmCoinEnum for UtxoStandardCoin {
    fn try_from_mm_coin(coin: MmCoinEnum) -> Option<Self>
    where
        Self: Sized,
    {
        match coin {
            MmCoinEnum::UtxoCoin(coin) => Some(coin),
            _ => None,
        }
    }
}

impl TryFromCoinProtocol for LightningProtocolConf {
    fn try_from_coin_protocol(proto: CoinProtocol) -> Result<Self, MmError<CoinProtocol>>
    where
        Self: Sized,
    {
        match proto {
            CoinProtocol::LIGHTNING {
                platform,
                network,
                confirmations,
            } => Ok(LightningProtocolConf {
                platform_coin_ticker: platform,
                network,
                confirmations,
            }),
            proto => MmError::err(proto),
        }
    }
}

impl L2ProtocolParams for LightningProtocolConf {
    fn platform_coin_ticker(&self) -> &str { &self.platform_coin_ticker }
}

#[derive(Clone, Debug, Deserialize)]
pub struct LightningActivationParams {
    // The listening port for the p2p LN node
    pub listening_port: Option<u16>,
    // Printable human-readable string to describe this node to other users.
    pub name: String,
    // Node's HEX color. This is used for showing the node in a network graph with the desired color.
    pub color: Option<String>,
    // The number of payment retries that should be done before considering a payment failed or partially failed.
    pub payment_retries: Option<usize>,
    // Node's backup path for channels and other data that requires backup.
    pub backup_path: Option<String>,
}

#[derive(Debug, Deserialize, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum LightningValidationErr {
    #[display(fmt = "Platform coin {} activated in {} mode", _0, _1)]
    UnexpectedMethod(String, String),
    #[display(fmt = "{} is only supported in {} mode", _0, _1)]
    UnsupportedMode(String, String),
    #[display(fmt = "Invalid request: {}", _0)]
    InvalidRequest(String),
    #[display(fmt = "Invalid address: {}", _0)]
    InvalidAddress(String),
}

/// What the platform coin's configuration lacks (CRD R41.2.6 row 9).
struct PlatformConfigurationError {
    platform_coin_ticker: String,
    err: String,
}

/// The platform's coin configuration must carry `avg_blocktime` (CRD R41.2.6 row 9).
fn check_platform_avg_blocktime(ctx: &MmArc, platform_ticker: &str) -> Result<(), MmError<PlatformConfigurationError>> {
    if coin_conf(ctx, platform_ticker)
        .get("avg_blocktime")
        .is_none_or(Json::is_null)
    {
        return MmError::err(PlatformConfigurationError {
            platform_coin_ticker: platform_ticker.to_owned(),
            err: "avg_blocktime is missing in the coin configuration".to_owned(),
        });
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize)]
pub struct LightningInitResult {
    platform_coin: String,
    address: String,
    balance: CoinBalance,
}

#[derive(Debug)]
pub enum LightningInitError {
    InvalidConfiguration(String),
    InvalidPlatformConfiguration { platform_coin_ticker: String, err: String },
    EnableLightningError(EnableLightningError),
    LightningValidationErr(LightningValidationErr),
    MyBalanceError(BalanceError),
    MyAddressError(String),
}

/// Task-compatible error that stringifies transient errors for Clone/Serialize/Display.
///
/// It is converted to the [`L2ActivationError`] of the task status (CRD ch.41 R41.2.7), which is what clients see.
#[derive(Clone, Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum LightningTaskError {
    #[display(fmt = "Invalid configuration: {}", _0)]
    InvalidConfiguration(String),
    #[display(fmt = "Invalid platform configuration for {}: {}", platform_coin_ticker, err)]
    InvalidPlatformConfiguration { platform_coin_ticker: String, err: String },
    #[display(fmt = "Lightning init failed: {}", _0)]
    EnableFailed(String),
    #[display(fmt = "Validation error: {}", _0)]
    Validation(String),
    /// The platform coin's backend could not be reached (CRD R41.2.7).
    #[display(fmt = "Transport error: {}", _0)]
    Transport(String),
    #[display(fmt = "Balance retrieval failed: {}", _0)]
    BalanceError(String),
    #[display(fmt = "Address error: {}", _0)]
    AddressError(String),
    /// The coin was registered by a concurrent activation first (CRD R41.2.7).
    #[display(fmt = "Layer 2 {} is already activated", _0)]
    L2IsAlreadyActivated(String),
    #[display(fmt = "Registration error: {}", _0)]
    RegistrationError(String),
    #[display(fmt = "Internal: {}", _0)]
    Internal(String),
}

impl From<RegisterCoinError> for LightningTaskError {
    fn from(e: RegisterCoinError) -> Self {
        match e {
            RegisterCoinError::CoinIsInitializedAlready { coin } => LightningTaskError::L2IsAlreadyActivated(coin),
            other => LightningTaskError::RegistrationError(other.to_string()),
        }
    }
}

impl From<LightningInitError> for LightningTaskError {
    fn from(e: LightningInitError) -> Self {
        match e {
            LightningInitError::InvalidConfiguration(s) => LightningTaskError::InvalidConfiguration(s),
            LightningInitError::InvalidPlatformConfiguration {
                platform_coin_ticker,
                err,
            } => LightningTaskError::InvalidPlatformConfiguration {
                platform_coin_ticker,
                err,
            },
            LightningInitError::EnableLightningError(e) => lightning_start_error(e),
            LightningInitError::LightningValidationErr(e) => LightningTaskError::Validation(e.to_string()),
            LightningInitError::MyBalanceError(e) => lightning_balance_error(e),
            LightningInitError::MyAddressError(e) => LightningTaskError::AddressError(e),
        }
    }
}

/// A failure to start the node: an unreachable platform backend is a transport error, anything else is internal
/// (CRD R41.2.7).
fn lightning_start_error(err: EnableLightningError) -> LightningTaskError {
    match err {
        EnableLightningError::RpcError(rpc_err) => LightningTaskError::Transport(rpc_err),
        other => LightningTaskError::EnableFailed(other.to_string()),
    }
}

/// Only a transport failure while reading the final balance is a transport error (CRD R41.2.7).
fn lightning_balance_error(err: BalanceError) -> LightningTaskError {
    match err {
        BalanceError::Transport(e) => LightningTaskError::Transport(e),
        other => LightningTaskError::BalanceError(other.to_string()),
    }
}

impl From<LightningTaskError> for L2ActivationError {
    fn from(e: LightningTaskError) -> Self {
        match e {
            LightningTaskError::InvalidConfiguration(s) => L2ActivationError::L2ConfigParseError(s),
            LightningTaskError::InvalidPlatformConfiguration {
                platform_coin_ticker,
                err,
            } => L2ActivationError::InvalidPlatformConfiguration {
                platform_coin_ticker,
                err,
            },
            LightningTaskError::Transport(s) => L2ActivationError::Transport(s),
            LightningTaskError::L2IsAlreadyActivated(ticker) => L2ActivationError::L2IsAlreadyActivated(ticker),
            LightningTaskError::EnableFailed(s)
            | LightningTaskError::Validation(s)
            | LightningTaskError::BalanceError(s)
            | LightningTaskError::AddressError(s)
            | LightningTaskError::RegistrationError(s)
            | LightningTaskError::Internal(s) => L2ActivationError::Internal(s),
        }
    }
}

impl HttpStatusCode for LightningTaskError {
    fn status_code(&self) -> StatusCode {
        match self {
            LightningTaskError::InvalidConfiguration(_) | LightningTaskError::Validation(_) => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// Progress indicator for task-based Lightning activation.
#[derive(Clone, Debug, Serialize)]
pub enum LightningActivationProgress {
    Initializing,
    ConnectingToPeers,
    SyncingChannels,
    FetchingBalance,
    ActivationComplete,
}

impl L2InitialStatus for LightningActivationProgress {
    fn initial_status() -> Self { LightningActivationProgress::Initializing }
}

/// Placeholder awaiting-action status (e.g. future hardware wallet confirmation).
#[derive(Clone, Debug, Serialize)]
pub struct LightningAwaitingAction {
    pub reason: String,
}

/// Placeholder user action type.
#[derive(Debug, Deserialize)]
pub enum LightningUserAction {
    ConfirmActivation,
}

impl From<LightningInitError> for EnableL2Error {
    fn from(err: LightningInitError) -> Self {
        match err {
            LightningInitError::InvalidConfiguration(err) => EnableL2Error::L2ConfigParseError(err),
            LightningInitError::InvalidPlatformConfiguration {
                platform_coin_ticker,
                err,
            } => EnableL2Error::InvalidPlatformConfiguration {
                platform_coin_ticker,
                err,
            },
            LightningInitError::EnableLightningError(enable_err) => match enable_err {
                EnableLightningError::RpcError(rpc_err) => EnableL2Error::Transport(rpc_err),
                enable_error => EnableL2Error::Internal(enable_error.to_string()),
            },
            LightningInitError::LightningValidationErr(req_err) => EnableL2Error::Internal(req_err.to_string()),
            LightningInitError::MyBalanceError(balance_err) => match balance_err {
                BalanceError::Transport(e) => EnableL2Error::Transport(e),
                balance_error => EnableL2Error::Internal(balance_error.to_string()),
            },
            LightningInitError::MyAddressError(e) => EnableL2Error::Internal(e),
        }
    }
}

impl From<EnableLightningError> for LightningInitError {
    fn from(err: EnableLightningError) -> Self { LightningInitError::EnableLightningError(err) }
}

impl From<LightningValidationErr> for LightningInitError {
    fn from(err: LightningValidationErr) -> Self { LightningInitError::LightningValidationErr(err) }
}

#[async_trait]
impl L2ActivationOps for LightningCoin {
    type PlatformCoin = UtxoStandardCoin;
    type ActivationParams = LightningActivationParams;
    type ProtocolInfo = LightningProtocolConf;
    type ValidatedParams = LightningParams;
    type CoinConf = LightningCoinConf;
    type ActivationResult = LightningInitResult;
    type ActivationError = LightningInitError;

    fn coin_conf_from_json(json: Json) -> Result<Self::CoinConf, MmError<Self::ActivationError>> {
        json::from_value::<LightningCoinConf>(json)
            .map_to_mm(|e| LightningInitError::InvalidConfiguration(e.to_string()))
    }

    fn validate_platform_configuration(
        ctx: &MmArc,
        platform_coin: &Self::PlatformCoin,
    ) -> Result<(), MmError<Self::ActivationError>> {
        // Channel funding transactions need to spend segwit outputs
        // and while the witness script can be generated from pubkey and be used
        // it's better for the coin to be enabled in segwit to check if balance is enough for funding transaction, etc...
        if !platform_coin.addr_format().is_segwit() {
            return MmError::err(
                LightningValidationErr::UnsupportedMode("Lightning network".into(), "segwit".into()).into(),
            );
        }
        check_platform_avg_blocktime(ctx, platform_coin.ticker()).mm_err(|e| {
            LightningInitError::InvalidPlatformConfiguration {
                platform_coin_ticker: e.platform_coin_ticker,
                err: e.err,
            }
        })
    }

    fn validate_activation_params(
        activation_params: Self::ActivationParams,
    ) -> Result<Self::ValidatedParams, MmError<Self::ActivationError>> {
        // Docs vs reference (CRD ch.41 R41.2.4): the public `LightningActivationParams` table states a colour
        // default of `2b6680`; the reference defaults to `000000`. Reloaded implements the documented `2b6680`
        // by maintainer decision (2026-10-11): the docs are explicit and the value is cosmetic, it is only the
        // node's colour in the network graph.
        if activation_params.name.len() > 32 {
            return MmError::err(
                LightningValidationErr::InvalidRequest("Node name length can't be more than 32 characters".into())
                    .into(),
            );
        }
        let mut node_name = [b' '; 32];
        node_name[0..activation_params.name.len()].copy_from_slice(activation_params.name.as_bytes());

        let mut node_color = [0u8; 3];
        hex::decode_to_slice(
            activation_params.color.unwrap_or_else(|| DEFAULT_NODE_COLOR.into()),
            &mut node_color as &mut [u8],
        )
        .map_to_mm(|_| LightningValidationErr::InvalidRequest("Invalid Hex Color".into()))
        .mm_err(Into::into)?;

        let listening_port = activation_params.listening_port.unwrap_or(DEFAULT_LISTENING_PORT);

        Ok(LightningParams {
            listening_port,
            node_name,
            node_color,
            payment_retries: activation_params.payment_retries,
            backup_path: activation_params.backup_path,
        })
    }

    async fn enable_l2(
        ctx: &MmArc,
        platform_coin: Self::PlatformCoin,
        validated_params: Self::ValidatedParams,
        protocol_conf: Self::ProtocolInfo,
        coin_conf: Self::CoinConf,
    ) -> Result<(Self, Self::ActivationResult), MmError<Self::ActivationError>> {
        let lightning_coin = start_lightning(ctx, platform_coin.clone(), protocol_conf, coin_conf, validated_params)
            .await
            .mm_err(Into::into)?;
        let address = lightning_coin
            .my_address()
            .map_to_mm(LightningInitError::MyAddressError)?;
        let balance = lightning_coin
            .my_balance()
            .compat()
            .await
            .mm_err(LightningInitError::MyBalanceError)?;
        let init_result = LightningInitResult {
            platform_coin: platform_coin.ticker().into(),
            address,
            balance,
        };
        Ok((lightning_coin, init_result))
    }
}

/// Helper: run core Lightning init logic, returning the coin and result.
/// Shared between the simple enable_l2 path and the task-based init_l2 path.
async fn lightning_init_core(
    ctx: &MmArc,
    platform_coin: UtxoStandardCoin,
    validated_params: LightningParams,
    protocol_conf: LightningProtocolConf,
    coin_conf: LightningCoinConf,
) -> Result<(LightningCoin, LightningInitResult), MmError<LightningTaskError>> {
    let lightning_coin = start_lightning(ctx, platform_coin.clone(), protocol_conf, coin_conf, validated_params)
        .await
        .map_err(|e| MmError::new(lightning_start_error(e.into_inner())))?;
    let address = lightning_coin
        .my_address()
        .map_to_mm(LightningTaskError::AddressError)?;
    let balance = lightning_coin
        .my_balance()
        .compat()
        .await
        .map_err(|e| MmError::new(lightning_balance_error(e.into_inner())))?;
    let init_result = LightningInitResult {
        platform_coin: platform_coin.ticker().into(),
        address,
        balance,
    };
    Ok((lightning_coin, init_result))
}

#[async_trait]
impl InitL2ActivationOps for LightningCoin {
    type PlatformCoin = UtxoStandardCoin;
    type ActivationParams = LightningActivationParams;
    type ProtocolInfo = LightningProtocolConf;
    type ValidatedParams = LightningParams;
    type CoinConf = LightningCoinConf;
    type ActivationResult = LightningInitResult;
    type ActivationError = LightningTaskError;
    type InProgressStatus = LightningActivationProgress;
    type AwaitingStatus = LightningAwaitingAction;
    type UserAction = LightningUserAction;

    fn rpc_task_manager(activation_ctx: &CoinsActivationContext) -> &L2TaskManagerShared<Self> {
        &activation_ctx.init_lightning_task_manager
    }

    fn coin_conf_from_json(json: Json) -> Result<Self::CoinConf, MmError<Self::ActivationError>> {
        json::from_value::<LightningCoinConf>(json)
            .map_to_mm(|e| LightningTaskError::InvalidConfiguration(e.to_string()))
    }

    fn validate_platform_configuration(
        ctx: &MmArc,
        platform_coin: &Self::PlatformCoin,
    ) -> Result<(), MmError<Self::ActivationError>> {
        if !platform_coin.addr_format().is_segwit() {
            return MmError::err(LightningTaskError::Validation(
                "Lightning network requires segwit address format".into(),
            ));
        }
        check_platform_avg_blocktime(ctx, platform_coin.ticker()).mm_err(|e| {
            LightningTaskError::InvalidPlatformConfiguration {
                platform_coin_ticker: e.platform_coin_ticker,
                err: e.err,
            }
        })
    }

    fn validate_activation_params(
        activation_params: Self::ActivationParams,
    ) -> Result<Self::ValidatedParams, MmError<Self::ActivationError>> {
        // Colour default `2b6680` as the public docs table states, not the reference's `000000`: see the one-shot
        // twin above (CRD ch.41 R41.2.4).
        if activation_params.name.len() > 32 {
            return MmError::err(LightningTaskError::Validation(
                "Node name length can't be more than 32 characters".into(),
            ));
        }
        let mut node_name = [b' '; 32];
        node_name[0..activation_params.name.len()].copy_from_slice(activation_params.name.as_bytes());

        let mut node_color = [0u8; 3];
        hex::decode_to_slice(
            activation_params.color.unwrap_or_else(|| DEFAULT_NODE_COLOR.into()),
            &mut node_color as &mut [u8],
        )
        .map_to_mm(|_| LightningTaskError::Validation("Invalid Hex Color".into()))?;

        let listening_port = activation_params.listening_port.unwrap_or(DEFAULT_LISTENING_PORT);

        Ok(LightningParams {
            listening_port,
            node_name,
            node_color,
            payment_retries: activation_params.payment_retries,
            backup_path: activation_params.backup_path,
        })
    }

    async fn init_l2(
        ctx: &MmArc,
        platform_coin: Self::PlatformCoin,
        validated_params: Self::ValidatedParams,
        protocol_conf: Self::ProtocolInfo,
        coin_conf: Self::CoinConf,
        _task_handle: &RpcTaskHandle<crate::l2::L2ActivationTask<Self>>,
    ) -> Result<(Self, Self::ActivationResult), MmError<Self::ActivationError>> {
        lightning_init_core(ctx, platform_coin, validated_params, protocol_conf, coin_conf).await
    }
}

#[cfg(test)] mod wire_tests;
