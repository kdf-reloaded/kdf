//! Cosmos staking operations on [`TendermintCoin`].
//!
//! Provides delegate, undelegate, claim-rewards, as well as read-only
//! queries for validators, delegations and ongoing undelegations.

use super::rpc::*;
use super::tendermint_types::*;
use crate::rpc_command::tendermint::staking::{ClaimRewardsPayload, Delegation, DelegationPayload,
                                              DelegationsQueryResponse, Undelegation, UndelegationEntry,
                                              UndelegationsQueryResponse, ValidatorStatus};
use crate::utxo::sat_from_big_decimal;
use crate::utxo::utxo_common::big_decimal_from_sat_unsigned;
use crate::{DelegationError, MarketCoinOps, TransactionDetails, TransactionType, TxFeeDetails};
use bigdecimal::BigDecimal;
use common::PagingOptions;
use cosmrs::distribution::MsgWithdrawDelegatorReward;
use cosmrs::proto::cosmos::base::query::v1beta1::PageRequest;
use cosmrs::proto::cosmos::distribution::v1beta1::{QueryDelegationRewardsRequest, QueryDelegationRewardsResponse};
use cosmrs::proto::cosmos::staking::v1beta1::{QueryDelegationRequest, QueryDelegationResponse,
                                              QueryDelegatorDelegationsRequest, QueryDelegatorDelegationsResponse,
                                              QueryDelegatorUnbondingDelegationsRequest,
                                              QueryDelegatorUnbondingDelegationsResponse, QueryValidatorsRequest,
                                              QueryValidatorsResponse as QueryValidatorsResponseProto};
use cosmrs::proto::prost::Message;
use cosmrs::staking::{MsgDelegate, MsgUndelegate, QueryValidatorsResponse, Validator};
use cosmrs::tx::Msg;
use cosmrs::{AccountId, Coin as CosmosCoin, Denom};
use futures::compat::Future01CompatExt;
use mm2_err_handle::prelude::*;
use num_traits::Zero;
use rpc::v1::types::Bytes as BytesJson;
use std::str::FromStr;
use std::time::Duration;

use super::tendermint_helpers::TendermintCommons;

// ————————————————————————————————————————————————————————————————
// ABCI Staking Paths
// ————————————————————————————————————————————————————————————————

const ABCI_VALIDATORS_PATH: &str = "/cosmos.staking.v1beta1.Query/Validators";
const ABCI_DELEGATION_PATH: &str = "/cosmos.staking.v1beta1.Query/Delegation";
const ABCI_DELEGATOR_DELEGATIONS_PATH: &str = "/cosmos.staking.v1beta1.Query/DelegatorDelegations";
const ABCI_DELEGATOR_UNDELEGATIONS_PATH: &str = "/cosmos.staking.v1beta1.Query/DelegatorUnbondingDelegations";
const ABCI_DELEGATION_REWARDS_PATH: &str = "/cosmos.distribution.v1beta1.Query/DelegationRewards";

// ————————————————————————————————————————————————————————————————
// Helpers
// ————————————————————————————————————————————————————————————————

/// Cosmos `DecCoin` stores amounts in 18-decimal representation.
/// Convert to a human-readable `BigDecimal` by shifting by `decimals` places.
fn extract_big_decimal_from_dec_coin(
    dec_coin: &cosmrs::proto::cosmos::base::v1beta1::DecCoin,
    decimals: u32,
) -> Result<BigDecimal, String> {
    const DEC_COIN_PRECISION: u32 = 18;

    let raw: BigDecimal = dec_coin
        .amount
        .parse::<BigDecimal>()
        .map_err(|e| format!("Failed to parse DecCoin amount: {e}"))?;

    // DecCoin values carry 18 implicit fractional digits
    let shifted = raw / BigDecimal::from(10u64.pow(DEC_COIN_PRECISION));
    // Now shift from base-unit to display-unit
    Ok(shifted / BigDecimal::from(10u64.pow(decimals)))
}

// ————————————————————————————————————————————————————————————————
// Read-only Queries
// ————————————————————————————————————————————————————————————————

impl TendermintCoin {
    /// Fetch the list of validators, optionally filtered by bonding status.
    pub(crate) async fn validators_list(
        &self,
        filter_status: ValidatorStatus,
        paging: PagingOptions,
    ) -> MmResult<Vec<Validator>, TendermintCoinRpcError> {
        let request = QueryValidatorsRequest {
            status: filter_status.to_string(),
            pagination: Some(PageRequest {
                key: vec![],
                offset: ((paging.page_number.get() - 1usize) * paging.limit) as u64,
                limit: paging.limit as u64,
                count_total: false,
                reverse: false,
            }),
        };

        let raw_response = self
            .rpc_client()
            .await?
            .abci_query(
                Some(ABCI_VALIDATORS_PATH.to_owned()),
                request.encode_to_vec(),
                ABCI_REQUEST_HEIGHT,
                ABCI_REQUEST_PROVE,
            )
            .await?;

        let decoded = QueryValidatorsResponseProto::decode(raw_response.value.as_slice())?;
        let typed = QueryValidatorsResponse::try_from(decoded)
            .map_err(|e| TendermintCoinRpcError::InternalError(e.to_string()))?;

        Ok(typed.validators)
    }

    /// Retrieve the amount currently delegated to a single validator.
    async fn get_delegated_amount(&self, validator_addr: &AccountId) -> MmResult<(BigDecimal, u64), DelegationError> {
        let delegator_addr = self.account_id.to_string();
        let validator_addr_str = validator_addr.to_string();

        let request = QueryDelegationRequest {
            delegator_addr: delegator_addr.clone(),
            validator_addr: validator_addr_str.clone(),
        };

        let raw_response = self
            .rpc_client()
            .await
            .map_mm_err()?
            .abci_query(
                Some(ABCI_DELEGATION_PATH.to_owned()),
                request.encode_to_vec(),
                ABCI_REQUEST_HEIGHT,
                ABCI_REQUEST_PROVE,
            )
            .await
            .map_err(|e| DelegationError::Transport(e.to_string()))?;

        let decoded = QueryDelegationResponse::decode(raw_response.value.as_slice())
            .map_err(|e| DelegationError::InternalError(e.to_string()))?;

        let delegation_response = match decoded.delegation_response {
            Some(dr) => dr,
            None => {
                return MmError::err(DelegationError::CanNotUndelegate {
                    delegator_addr,
                    validator_addr: validator_addr_str,
                })
            },
        };

        let balance = delegation_response.balance.ok_or_else(|| {
            DelegationError::Transport(format!(
                "Unexpected response from '{ABCI_DELEGATION_PATH}'; balance field should not be empty."
            ))
        })?;

        let uamount = u64::from_str(&balance.amount).map_err(|e| DelegationError::InternalError(e.to_string()))?;

        Ok((big_decimal_from_sat_unsigned(uamount, self.decimals()), uamount))
    }

    /// Retrieve the pending staking reward for a single validator.
    async fn get_delegation_reward_amount(&self, validator_addr: &AccountId) -> MmResult<BigDecimal, DelegationError> {
        let delegator_address = self.account_id.to_string();
        let validator_address = validator_addr.to_string();

        let query = QueryDelegationRewardsRequest {
            delegator_address,
            validator_address,
        };

        let raw_response = self
            .rpc_client()
            .await
            .map_mm_err()?
            .abci_query(
                Some(ABCI_DELEGATION_REWARDS_PATH.to_owned()),
                query.encode_to_vec(),
                ABCI_REQUEST_HEIGHT,
                ABCI_REQUEST_PROVE,
            )
            .await
            .map_err(|e| DelegationError::Transport(e.to_string()))?;

        let decoded = QueryDelegationRewardsResponse::decode(raw_response.value.as_slice())
            .map_err(|e| DelegationError::InternalError(e.to_string()))?;

        match decoded
            .rewards
            .iter()
            .find(|c| c.denom == self.protocol_info.denom.to_string())
        {
            Some(dec_coin) => extract_big_decimal_from_dec_coin(dec_coin, self.protocol_info.decimals as u32)
                .map_to_mm(|e| DelegationError::InternalError(e)),
            None => MmError::err(DelegationError::NothingToClaim {
                coin: self.ticker.clone(),
            }),
        }
    }

    /// List all active delegations for the current account.
    pub(crate) async fn delegations_list(
        &self,
        paging: PagingOptions,
    ) -> MmResult<DelegationsQueryResponse, TendermintCoinRpcError> {
        let request = QueryDelegatorDelegationsRequest {
            delegator_addr: self.account_id.to_string(),
            pagination: Some(PageRequest {
                key: vec![],
                offset: ((paging.page_number.get() - 1usize) * paging.limit) as u64,
                limit: paging.limit as u64,
                count_total: false,
                reverse: false,
            }),
        };

        let raw_response = self
            .rpc_client()
            .await?
            .abci_query(
                Some(ABCI_DELEGATOR_DELEGATIONS_PATH.to_owned()),
                request.encode_to_vec(),
                ABCI_REQUEST_HEIGHT,
                ABCI_REQUEST_PROVE,
            )
            .await?;

        let decoded = QueryDelegatorDelegationsResponse::decode(raw_response.value.as_slice())?;
        let self_clone = self.clone();

        let mut delegations = Vec::new();
        for resp in decoded.delegation_responses {
            let Some(delegation) = resp.delegation else { continue };
            let Some(balance) = resp.balance else { continue };

            let account_id = AccountId::from_str(&delegation.validator_address)
                .map_err(|e| TendermintCoinRpcError::InternalError(e.to_string()))?;

            let reward_amount = match self_clone.get_delegation_reward_amount(&account_id).await {
                Ok(reward) => reward,
                Err(e) => match e.get_inner() {
                    DelegationError::NothingToClaim { .. } => BigDecimal::zero(),
                    _ => return MmError::err(TendermintCoinRpcError::InvalidResponse(e.to_string())),
                },
            };

            let amount = balance
                .amount
                .parse::<u64>()
                .map_err(|e| TendermintCoinRpcError::InternalError(e.to_string()))?;

            delegations.push(Delegation {
                validator_address: delegation.validator_address,
                delegated_amount: big_decimal_from_sat_unsigned(amount, self_clone.decimals()),
                reward_amount,
            });
        }

        Ok(DelegationsQueryResponse { delegations })
    }

    /// List all ongoing (pending) undelegations for the current account.
    pub(crate) async fn ongoing_undelegations_list(
        &self,
        paging: PagingOptions,
    ) -> MmResult<UndelegationsQueryResponse, TendermintCoinRpcError> {
        let request = QueryDelegatorUnbondingDelegationsRequest {
            delegator_addr: self.account_id.to_string(),
            pagination: Some(PageRequest {
                key: vec![],
                offset: ((paging.page_number.get() - 1usize) * paging.limit) as u64,
                limit: paging.limit as u64,
                count_total: false,
                reverse: false,
            }),
        };

        let raw_response = self
            .rpc_client()
            .await?
            .abci_query(
                Some(ABCI_DELEGATOR_UNDELEGATIONS_PATH.to_owned()),
                request.encode_to_vec(),
                ABCI_REQUEST_HEIGHT,
                ABCI_REQUEST_PROVE,
            )
            .await?;

        let decoded = QueryDelegatorUnbondingDelegationsResponse::decode(raw_response.value.as_slice())?;
        let ongoing_undelegations = decoded
            .unbonding_responses
            .into_iter()
            .map(|r| {
                let entries = r
                    .entries
                    .into_iter()
                    .filter_map(|e| {
                        let balance: u64 = e.balance.parse().ok()?;
                        Some(UndelegationEntry {
                            creation_height: e.creation_height,
                            completion_datetime: e.completion_time?.to_string(),
                            balance: big_decimal_from_sat_unsigned(balance, self.decimals()),
                        })
                    })
                    .collect();

                Undelegation {
                    validator_address: r.validator_address,
                    entries,
                }
            })
            .collect();

        Ok(UndelegationsQueryResponse { ongoing_undelegations })
    }
}

// ————————————————————————————————————————————————————————————————
// Write Operations (Delegate / Undelegate / Claim)
// ————————————————————————————————————————————————————————————————

impl TendermintCoin {
    /// Send a delegation (stake) transaction.
    pub(crate) async fn delegate(&self, req: DelegationPayload) -> MmResult<TransactionDetails, DelegationError> {
        fn make_msg(
            delegator: AccountId,
            validator: AccountId,
            denom: Denom,
            amount: u128,
        ) -> Result<cosmrs::Any, cosmrs::ErrorReport> {
            MsgDelegate {
                delegator_address: delegator,
                validator_address: validator,
                amount: CosmosCoin { denom, amount },
            }
            .to_any()
        }

        let validator_address =
            AccountId::from_str(&req.validator_address).map_to_mm(|e| DelegationError::AddressError(e.to_string()))?;

        let balance_u64 = self
            .account_balance_for_denom(&self.account_id, self.protocol_info.denom.to_string())
            .await
            .map_mm_err()?;
        let balance_dec = big_decimal_from_sat_unsigned(balance_u64, self.decimals());

        let amount_u64 = if req.max {
            balance_u64
        } else {
            sat_from_big_decimal(&req.amount, self.protocol_info.decimals)
                .map_err(|e| DelegationError::InternalError(e.to_string()))?
        };

        // Simulate to predict fee
        let sim_msg = make_msg(
            self.account_id.clone(),
            validator_address.clone(),
            self.protocol_info.denom.clone(),
            amount_u64.into(),
        )
        .map_err(|e| DelegationError::InternalError(e.to_string()))?;

        let timeout_height = self
            .current_block()
            .compat()
            .await
            .map_to_mm(DelegationError::Transport)?
            + TIMEOUT_HEIGHT_DELTA;

        let fee = self
            .calculate_fee(sim_msg, timeout_height, &req.memo, req.fee)
            .await
            .map_mm_err()?;

        let fee_amount_u64 = fee
            .amount
            .first()
            .map(|c| u64::try_from(c.amount).unwrap_or(0))
            .unwrap_or(0);
        let fee_amount_dec = big_decimal_from_sat_unsigned(fee_amount_u64, self.decimals());
        let gas_limit = fee.gas_limit;

        // Determine final send amount + total
        let (send_u64, total_amount) = if req.max {
            if balance_u64 < fee_amount_u64 {
                return MmError::err(DelegationError::NotSufficientBalance {
                    coin: self.ticker.clone(),
                    available: balance_dec,
                    required: fee_amount_dec,
                });
            }
            let send = balance_u64 - fee_amount_u64;
            (send, balance_dec.clone())
        } else {
            let total = &req.amount + &fee_amount_dec;
            if balance_dec < total {
                return MmError::err(DelegationError::NotSufficientBalance {
                    coin: self.ticker.clone(),
                    available: balance_dec,
                    required: total,
                });
            }
            (amount_u64, total)
        };

        let msg = make_msg(
            self.account_id.clone(),
            validator_address.clone(),
            self.protocol_info.denom.clone(),
            send_u64.into(),
        )
        .map_err(|e| DelegationError::InternalError(e.to_string()))?;

        let (tx_id, tx_raw) = self
            .common_send_raw_tx_bytes(msg, fee, timeout_height, &req.memo, Duration::from_secs(30))
            .await
            .map_err(|e| DelegationError::InternalError(format!("{e:?}")))?;

        let tx_hex: BytesJson = tx_raw
            .to_bytes()
            .map_err(|e| DelegationError::InternalError(e.to_string()))?
            .into();

        let tx_details = TransactionDetails {
            tx_hex: tx_hex.clone(),
            tx_hash: tx_id,
            from: vec![self.account_id.to_string()],
            to: vec![req.validator_address],
            my_balance_change: &BigDecimal::default() - &total_amount,
            spent_by_me: total_amount.clone(),
            total_amount,
            received_by_me: BigDecimal::default(),
            block_height: 0,
            timestamp: 0,
            fee_details: Some(TxFeeDetails::Tendermint(TendermintFeeDetails {
                coin: self.ticker.clone(),
                amount: fee_amount_dec,
                uamount: fee_amount_u64,
                gas_limit,
            })),
            coin: self.ticker.to_string(),
            internal_id: tx_hex.clone(),
            kmd_rewards: None,
            transaction_type: TransactionType::StakingDelegation,
        };
        self.publish_tx_history_record(self.ticker(), &tx_details);
        Ok(tx_details)
    }

    /// Send an undelegation (unstake) transaction.
    pub(crate) async fn undelegate(&self, req: DelegationPayload) -> MmResult<TransactionDetails, DelegationError> {
        fn make_msg(
            delegator: AccountId,
            validator: AccountId,
            denom: Denom,
            amount: u128,
        ) -> Result<cosmrs::Any, cosmrs::ErrorReport> {
            MsgUndelegate {
                delegator_address: delegator,
                validator_address: validator,
                amount: CosmosCoin { denom, amount },
            }
            .to_any()
        }

        let validator_address =
            AccountId::from_str(&req.validator_address).map_to_mm(|e| DelegationError::AddressError(e.to_string()))?;

        let (total_delegated_dec, total_delegated_u) = self.get_delegated_amount(&validator_address).await?;

        let uamount = if req.max {
            total_delegated_u
        } else {
            if req.amount > total_delegated_dec {
                return MmError::err(DelegationError::TooMuchToUndelegate {
                    available: total_delegated_dec,
                    requested: req.amount,
                });
            }
            sat_from_big_decimal(&req.amount, self.protocol_info.decimals)
                .map_err(|e| DelegationError::InternalError(e.to_string()))?
        };

        let msg = make_msg(
            self.account_id.clone(),
            validator_address.clone(),
            self.protocol_info.denom.clone(),
            uamount.into(),
        )
        .map_err(|e| DelegationError::InternalError(e.to_string()))?;

        let timeout_height = self
            .current_block()
            .compat()
            .await
            .map_to_mm(DelegationError::Transport)?
            + TIMEOUT_HEIGHT_DELTA;

        let fee = self
            .calculate_fee(msg.clone(), timeout_height, &req.memo, req.fee)
            .await
            .map_mm_err()?;

        let fee_amount_u64 = fee
            .amount
            .first()
            .map(|c| u64::try_from(c.amount).unwrap_or(0))
            .unwrap_or(0);
        let fee_amount_dec = big_decimal_from_sat_unsigned(fee_amount_u64, self.decimals());
        let gas_limit = fee.gas_limit;

        let my_balance = self.my_balance().compat().await.map_mm_err()?.spendable;
        if fee_amount_dec > my_balance {
            return MmError::err(DelegationError::NotSufficientBalance {
                coin: self.ticker.clone(),
                available: my_balance,
                required: fee_amount_dec,
            });
        }

        let (tx_id, tx_raw) = self
            .common_send_raw_tx_bytes(msg, fee, timeout_height, &req.memo, Duration::from_secs(30))
            .await
            .map_err(|e| DelegationError::InternalError(format!("{e:?}")))?;

        let tx_hex: BytesJson = tx_raw
            .to_bytes()
            .map_err(|e| DelegationError::InternalError(e.to_string()))?
            .into();

        let tx_details = TransactionDetails {
            tx_hex: tx_hex.clone(),
            tx_hash: tx_id,
            from: vec![self.account_id.to_string()],
            to: vec![],
            my_balance_change: &BigDecimal::default() - &fee_amount_dec,
            spent_by_me: fee_amount_dec.clone(),
            total_amount: fee_amount_dec.clone(),
            received_by_me: BigDecimal::default(),
            block_height: 0,
            timestamp: 0,
            fee_details: Some(TxFeeDetails::Tendermint(TendermintFeeDetails {
                coin: self.ticker.clone(),
                amount: fee_amount_dec,
                uamount: fee_amount_u64,
                gas_limit,
            })),
            coin: self.ticker.to_string(),
            internal_id: tx_hex.clone(),
            kmd_rewards: None,
            transaction_type: TransactionType::RemoveDelegation,
        };
        self.publish_tx_history_record(self.ticker(), &tx_details);
        Ok(tx_details)
    }

    /// Claim staking rewards from a validator.
    pub(crate) async fn claim_staking_rewards(
        &self,
        req: ClaimRewardsPayload,
    ) -> MmResult<TransactionDetails, DelegationError> {
        let validator_address =
            AccountId::from_str(&req.validator_address).map_to_mm(|e| DelegationError::AddressError(e.to_string()))?;

        let msg = MsgWithdrawDelegatorReward {
            delegator_address: self.account_id.clone(),
            validator_address: validator_address.clone(),
        }
        .to_any()
        .map_err(|e| DelegationError::InternalError(e.to_string()))?;

        let reward_amount = self.get_delegation_reward_amount(&validator_address).await?;

        if reward_amount.is_zero() {
            return MmError::err(DelegationError::NothingToClaim {
                coin: self.ticker.clone(),
            });
        }

        let timeout_height = self
            .current_block()
            .compat()
            .await
            .map_to_mm(DelegationError::Transport)?
            + TIMEOUT_HEIGHT_DELTA;

        let fee = self
            .calculate_fee(msg.clone(), timeout_height, &req.memo, req.fee)
            .await
            .map_mm_err()?;

        let fee_amount_u64 = fee
            .amount
            .first()
            .map(|c| u64::try_from(c.amount).unwrap_or(0))
            .unwrap_or(0);
        let fee_amount_dec = big_decimal_from_sat_unsigned(fee_amount_u64, self.decimals());
        let gas_limit = fee.gas_limit;

        let my_balance = self.my_balance().compat().await.map_mm_err()?.spendable;
        if fee_amount_dec > my_balance {
            return MmError::err(DelegationError::NotSufficientBalance {
                coin: self.ticker.clone(),
                available: my_balance,
                required: fee_amount_dec,
            });
        }

        if !req.force && fee_amount_dec > reward_amount {
            return MmError::err(DelegationError::UnprofitableReward {
                reward: reward_amount.clone(),
                fee: fee_amount_dec.clone(),
            });
        }

        let (tx_id, tx_raw) = self
            .common_send_raw_tx_bytes(msg, fee, timeout_height, &req.memo, Duration::from_secs(30))
            .await
            .map_err(|e| DelegationError::InternalError(format!("{e:?}")))?;

        let tx_hex: BytesJson = tx_raw
            .to_bytes()
            .map_err(|e| DelegationError::InternalError(e.to_string()))?
            .into();

        let tx_details = TransactionDetails {
            tx_hex: tx_hex.clone(),
            tx_hash: tx_id,
            from: vec![validator_address.to_string()],
            to: vec![self.account_id.to_string()],
            my_balance_change: &reward_amount - &fee_amount_dec,
            spent_by_me: fee_amount_dec.clone(),
            total_amount: reward_amount.clone(),
            received_by_me: reward_amount,
            block_height: 0,
            timestamp: 0,
            fee_details: Some(TxFeeDetails::Tendermint(TendermintFeeDetails {
                coin: self.ticker.clone(),
                amount: fee_amount_dec,
                uamount: fee_amount_u64,
                gas_limit,
            })),
            coin: self.ticker.to_string(),
            internal_id: tx_hex.clone(),
            kmd_rewards: None,
            transaction_type: TransactionType::ClaimDelegationRewards,
        };
        self.publish_tx_history_record(self.ticker(), &tx_details);
        Ok(tx_details)
    }
}
