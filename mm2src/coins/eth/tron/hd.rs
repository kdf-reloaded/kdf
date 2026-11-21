//! The used-address rule of the TRON HD wallet (CRD ch.21 R21.10B.4).
//!
//! An address of the wallet counts as used when the chain has created its account (a non-empty
//! `POST /wallet/getaccount` answer) or when any enabled TRC20 token has a non-zero balance at it. The scan for new
//! addresses and the activation scan stop after `gap_limit` addresses that are not used.
//!
//! The rule is the same when a scan is asked for a TRC20 ticker (the Reloaded note of 2026-11-19 in CRD ch.21):
//! receiving a TRC20 token does not create the account, so an address that holds only a token would otherwise be
//! missed.

use super::api::TronApiClient;
use super::balance::{trc20_balance_units, TronBalanceError};
use super::TronAddress;

use crate::eth::Erc20TokenInfo;
use crate::{BalanceError, BalanceResult};

use ethereum_types::Address;
use mm2_err_handle::prelude::*;

/// Decides whether an address of a TRON HD wallet is used. It holds the node client and the TRC20 tokens enabled on
/// the platform at the time the scanner was produced.
pub(crate) struct TronUsedAddressProbe {
    api: TronApiClient,
    tokens: Vec<Erc20TokenInfo>,
}

impl TronUsedAddressProbe {
    pub(crate) fn new(api: TronApiClient, tokens: Vec<Erc20TokenInfo>) -> Self { TronUsedAddressProbe { api, tokens } }

    /// Whether `address` is used.
    ///
    /// # Errors
    ///
    /// A failed node read is an error, never "not used": an unreachable node must not end a scan early.
    pub(crate) async fn is_used(&self, address: &Address) -> BalanceResult<bool> {
        let owner = TronAddress::from_evm_address(*address);
        let account = self
            .api
            .get_account(&owner)
            .await
            .map_to_mm(|e| BalanceError::from(TronBalanceError::Node(e)))?;
        if account.is_some() {
            return Ok(true);
        }
        for token in &self.tokens {
            let contract = TronAddress::from_evm_address(token.token_addr);
            let units = trc20_balance_units(&self.api, &contract, &owner)
                .await
                .map_to_mm(BalanceError::from)?;
            if !units.is_zero() {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
