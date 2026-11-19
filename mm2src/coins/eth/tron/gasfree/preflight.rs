//! The preflight decision of the gasless rail (CRD ch.63 §63.7, R2, R19).
//!
//! [`decide`] is a pure function of the provider's account record, the on-chain token balance of the custody
//! address (if it was read), the locally derived custody address, the token and the requested amount. It ends in
//! one of the three outcome classes of §63.7: proceed, wait, or one of four refusals.
//!
//! # Invariants
//!
//! - The custody-identity check comes before every other check: no figure is taken from a record that describes
//!   another account (R2).
//! - The wait decision is taken from the account record alone; it never needs the on-chain balance.
//! - All amounts are in the token's smallest unit.

use super::super::TronAddress;
use super::failure::{GaslessMember, RpcArm};
use super::provider_payloads::ProviderAccount;
use ethereum_types::U256;

/// The on-chain token balance of the custody address, as far as the caller has it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum CustodyBalance {
    /// The balance was not read.
    NotRead,
    /// The balance, in the token's smallest unit.
    Read(U256),
}

/// The figures a transfer that may proceed is signed with (§63.8 step 3).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Quote {
    pub transfer_fee: U256,
    /// Zero for an account that is already activated on chain.
    pub activation_fee: U256,
    /// `transfer_fee + activation_fee`.
    pub total_fee: U256,
    /// The nonce the provider's account record reports.
    pub nonce: u64,
}

/// Why the preflight refuses a transfer (§63.7). Nothing is signed after a refusal.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Refusal {
    /// The custody address in the account record is not the locally derived one: an untrustworthy provider
    /// answer (§63.9.2).
    CustodyMismatch,
    /// The account record lists no asset for the token's contract: the rail is unavailable (§63.9.2).
    TokenNotEnrolled,
    /// The asset's decimal count is not the activated token's: an untrustworthy provider answer (§63.9.2).
    PrecisionDisagreement,
    /// A funds test fails: the shared insufficient-balance error with these figures of the custody account
    /// (§63.9.2).
    Shortfall { available: U256, required: U256 },
}

/// The outcome of the preflight: exactly one of the three classes of §63.7.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PreflightDecision {
    /// The rail goes on to sign.
    Proceed(Quote),
    /// The provider withholds submit permission while an earlier transfer of the account is unsettled.
    Wait,
    Refuse(Refusal),
}

/// Why [`decide`] could not reach a decision.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PreflightIncomplete {
    /// The decision needs the on-chain balance of the custody address, which was not read: read it and ask again.
    BalanceNotRead,
    /// The sum of the requested amount and the provider's figures does not fit 256 bits.
    AmountOverflow,
}

impl PreflightDecision {
    /// The arm of the withdraw RPC the outcome is reported through (§63.9.2, R19); `None` for *proceed*.
    pub(crate) fn rpc_arm(&self) -> Option<RpcArm> {
        match self {
            PreflightDecision::Proceed(_) => None,
            PreflightDecision::Wait => Some(RpcArm::Gasless(GaslessMember::PendingTransfer)),
            PreflightDecision::Refuse(Refusal::TokenNotEnrolled) => Some(RpcArm::Gasless(GaslessMember::Unavailable)),
            PreflightDecision::Refuse(Refusal::CustodyMismatch | Refusal::PrecisionDisagreement) => {
                Some(RpcArm::Gasless(GaslessMember::InvalidProviderResponse))
            },
            PreflightDecision::Refuse(Refusal::Shortfall { .. }) => Some(RpcArm::NotSufficientBalance),
        }
    }
}

/// Decides whether a gasless transfer of `value` of the token at `token` (activated with `token_decimals`) may
/// proceed, from the provider's `account` record, the locally derived custody address `derived_custody` and the
/// on-chain `custody_balance` of the token (§63.7).
///
/// The account record's `accountAddress` is deliberately not compared with the user: the custody-address check
/// (R2) is what binds the record to this user.
///
/// The checks run in this order: the custody identity; the token's enrollment; its precision; the submit
/// permission; the funds tests. Only the funds tests read `custody_balance`.
///
/// # Errors
///
/// - [`PreflightIncomplete::BalanceNotRead`] when the funds tests are reached with [`CustodyBalance::NotRead`].
/// - [`PreflightIncomplete::AmountOverflow`] when a sum of the funds tests overflows.
pub(crate) fn decide(
    account: &ProviderAccount,
    custody_balance: CustodyBalance,
    derived_custody: &TronAddress,
    token: &TronAddress,
    token_decimals: u8,
    value: U256,
) -> Result<PreflightDecision, PreflightIncomplete> {
    // First of all: the record must describe the account whose custody address this build derives (R2).
    if account.gas_free_address != *derived_custody {
        return Ok(PreflightDecision::Refuse(Refusal::CustodyMismatch));
    }
    let Some(asset) = account.assets.iter().find(|asset| asset.token_address == *token) else {
        return Ok(PreflightDecision::Refuse(Refusal::TokenNotEnrolled));
    };
    if asset.decimal != u64::from(token_decimals) {
        return Ok(PreflightDecision::Refuse(Refusal::PrecisionDisagreement));
    }
    if !account.allow_submit {
        return Ok(PreflightDecision::Wait);
    }
    let CustodyBalance::Read(balance) = custody_balance else {
        return Err(PreflightIncomplete::BalanceNotRead);
    };

    // The activation fee counts only while the custody account is not activated on chain.
    let activation_fee = if account.active {
        U256::zero()
    } else {
        asset.activate_fee
    };
    let total_fee = asset
        .transfer_fee
        .checked_add(activation_fee)
        .ok_or(PreflightIncomplete::AmountOverflow)?;
    let required = value
        .checked_add(total_fee)
        .ok_or(PreflightIncomplete::AmountOverflow)?;

    if !account.active {
        // The test of a not-yet-activated account, evaluated first for it: the whole on-chain balance against
        // the requirement with the locked amount in it.
        let required_with_frozen = required
            .checked_add(asset.frozen)
            .ok_or(PreflightIncomplete::AmountOverflow)?;
        if balance < required_with_frozen {
            return Ok(PreflightDecision::Refuse(Refusal::Shortfall {
                available: balance,
                required: required_with_frozen,
            }));
        }
    }
    // The test of every account: the spendable balance, which is nothing when more is locked than is there.
    let spendable = balance.saturating_sub(asset.frozen);
    if spendable < required {
        return Ok(PreflightDecision::Refuse(Refusal::Shortfall {
            available: spendable,
            required,
        }));
    }

    Ok(PreflightDecision::Proceed(Quote {
        transfer_fee: asset.transfer_fee,
        activation_fee,
        total_fee,
        nonce: account.nonce,
    }))
}

/// ch.63 T7.
#[cfg(test)]
mod tests {
    use super::super::super::Network;
    use super::super::custody::derive_custody;
    use super::super::provider_payloads::AccountAsset;
    use super::super::test_vectors::{address, MESSAGES, USERS};
    use super::*;

    const DECIMALS: u8 = 6;
    const TRANSFER_FEE: u64 = 2_000_000;
    const ACTIVATE_FEE: u64 = 1_000_000;
    const VALUE: u64 = 10_000_000;

    fn token() -> TronAddress { address(MESSAGES[0].token) }

    fn custody() -> TronAddress { derive_custody(&Network::Nile, &address(USERS[0].address)).address }

    /// An account record of the first fixture user with the token enrolled.
    fn account(active: bool, frozen: u64) -> ProviderAccount {
        ProviderAccount {
            account_address: address(USERS[0].address),
            gas_free_address: custody(),
            active,
            nonce: 7,
            allow_submit: true,
            assets: vec![AccountAsset {
                token_address: token(),
                token_symbol: "USDT".to_owned(),
                activate_fee: U256::from(ACTIVATE_FEE),
                transfer_fee: U256::from(TRANSFER_FEE),
                decimal: u64::from(DECIMALS),
                frozen: U256::from(frozen),
            }],
        }
    }

    fn decide_with(
        account: &ProviderAccount,
        balance: CustodyBalance,
    ) -> Result<PreflightDecision, PreflightIncomplete> {
        decide(account, balance, &custody(), &token(), DECIMALS, U256::from(VALUE))
    }

    fn read(balance: u64) -> CustodyBalance { CustodyBalance::Read(U256::from(balance)) }

    fn shortfall(available: u64, required: u64) -> PreflightDecision {
        PreflightDecision::Refuse(Refusal::Shortfall {
            available: U256::from(available),
            required: U256::from(required),
        })
    }

    #[test]
    fn an_activated_account_with_funds_proceeds_without_an_activation_fee() {
        let decision = decide_with(&account(true, 0), read(VALUE + TRANSFER_FEE)).expect("a decision");
        assert_eq!(
            decision,
            PreflightDecision::Proceed(Quote {
                transfer_fee: U256::from(TRANSFER_FEE),
                activation_fee: U256::zero(),
                total_fee: U256::from(TRANSFER_FEE),
                nonce: 7,
            })
        );
        assert_eq!(decision.rpc_arm(), None);
    }

    #[test]
    fn a_not_activated_account_pays_the_activation_fee() {
        let needed = VALUE + TRANSFER_FEE + ACTIVATE_FEE;
        let decision = decide_with(&account(false, 0), read(needed)).expect("a decision");
        assert_eq!(
            decision,
            PreflightDecision::Proceed(Quote {
                transfer_fee: U256::from(TRANSFER_FEE),
                activation_fee: U256::from(ACTIVATE_FEE),
                total_fee: U256::from(TRANSFER_FEE + ACTIVATE_FEE),
                nonce: 7,
            })
        );
        // The balance that suffices for an activated account does not suffice here.
        let decision = decide_with(&account(false, 0), read(needed - 1)).expect("a decision");
        assert_eq!(decision, shortfall(needed - 1, needed));
    }

    #[test]
    fn a_custody_mismatch_is_refused_before_everything_else() {
        // Everything else about the record would refuse or wait too: the identity check still decides.
        let mut record = account(true, 0);
        record.gas_free_address = derive_custody(&Network::Mainnet, &address(USERS[0].address)).address;
        record.allow_submit = false;
        record.assets.clear();
        for balance in [CustodyBalance::NotRead, read(0), read(u64::MAX)] {
            let decision = decide_with(&record, balance).expect("a decision");
            assert_eq!(decision, PreflightDecision::Refuse(Refusal::CustodyMismatch));
            assert_eq!(
                decision.rpc_arm(),
                Some(RpcArm::Gasless(GaslessMember::InvalidProviderResponse))
            );
        }
    }

    #[test]
    fn a_token_the_record_does_not_list_is_not_enrolled() {
        let mut record = account(true, 0);
        record.assets[0].token_address = address(MESSAGES[1].token);
        let decision = decide_with(&record, CustodyBalance::NotRead).expect("a decision");
        assert_eq!(decision, PreflightDecision::Refuse(Refusal::TokenNotEnrolled));
        assert_eq!(decision.rpc_arm(), Some(RpcArm::Gasless(GaslessMember::Unavailable)));
        assert_eq!(GaslessMember::Unavailable.http_status(), 503);
    }

    #[test]
    fn another_decimal_count_is_a_precision_disagreement() {
        let mut record = account(true, 0);
        record.assets[0].decimal = 18;
        let decision = decide_with(&record, CustodyBalance::NotRead).expect("a decision");
        assert_eq!(decision, PreflightDecision::Refuse(Refusal::PrecisionDisagreement));
        assert_eq!(
            decision.rpc_arm(),
            Some(RpcArm::Gasless(GaslessMember::InvalidProviderResponse))
        );
        assert_eq!(GaslessMember::InvalidProviderResponse.http_status(), 502);
    }

    #[test]
    fn a_transfer_in_flight_waits_and_needs_no_chain_read() {
        let mut record = account(true, 0);
        record.allow_submit = false;
        // The decision is the same without a balance, with none and with plenty.
        for balance in [CustodyBalance::NotRead, read(0), read(u64::MAX)] {
            let decision = decide_with(&record, balance).expect("a decision without a chain read");
            assert_eq!(decision, PreflightDecision::Wait);
            assert_eq!(
                decision.rpc_arm(),
                Some(RpcArm::Gasless(GaslessMember::PendingTransfer))
            );
        }
        assert_eq!(GaslessMember::PendingTransfer.http_status(), 409);
    }

    #[test]
    fn the_funds_tests_need_the_balance() {
        assert_eq!(
            decide_with(&account(true, 0), CustodyBalance::NotRead),
            Err(PreflightIncomplete::BalanceNotRead)
        );
    }

    #[test]
    fn an_activated_account_is_short_by_its_spendable_balance() {
        let frozen = 3_000_000;
        let required = VALUE + TRANSFER_FEE;
        // Enough on chain, but part of it is locked.
        let decision = decide_with(&account(true, frozen), read(required + frozen - 1)).expect("a decision");
        assert_eq!(decision, shortfall(required - 1, required));
        assert_eq!(decision.rpc_arm(), Some(RpcArm::NotSufficientBalance));
        let decision = decide_with(&account(true, frozen), read(required + frozen)).expect("a decision");
        assert!(matches!(decision, PreflightDecision::Proceed(_)));
        // More locked than there is: nothing is spendable.
        let decision = decide_with(&account(true, frozen), read(frozen - 1)).expect("a decision");
        assert_eq!(decision, shortfall(0, required));
    }

    #[test]
    fn a_not_activated_account_is_short_by_its_whole_balance() {
        let frozen = 3_000_000;
        let required = VALUE + TRANSFER_FEE + ACTIVATE_FEE + frozen;
        // The figures are the whole on-chain balance and the requirement with the locked amount in it.
        let decision = decide_with(&account(false, frozen), read(required - 1)).expect("a decision");
        assert_eq!(decision, shortfall(required - 1, required));
        assert_eq!(decision.rpc_arm(), Some(RpcArm::NotSufficientBalance));
        let decision = decide_with(&account(false, frozen), read(required)).expect("a decision");
        assert!(matches!(decision, PreflightDecision::Proceed(_)));
    }

    #[test]
    fn a_sum_that_does_not_fit_is_not_a_decision() {
        let decision = decide(
            &account(true, 0),
            read(u64::MAX),
            &custody(),
            &token(),
            DECIMALS,
            U256::MAX,
        );
        assert_eq!(decision, Err(PreflightIncomplete::AmountOverflow));
    }
}
