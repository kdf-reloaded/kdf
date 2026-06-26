//! GasFree account preflight, availability decision, and fee math (§49.7).
//!
//! These are pure decision functions over already-gathered inputs (provider
//! account state, the on-chain TRC20 balance of the custody address, and the
//! local `CREATE2` derivation). The live fetching that gathers those inputs
//! lives in `withdraw.rs`; the decision categories below are the normative
//! contract (R2 address-mismatch safety stop; the balance/decimal/token gates).

use super::client::AccountInfo;
use super::error::GasFreeWithdrawError;
use crate::eth::tron::address::TronAddress;
use ethereum_types::U256;

/// A computed gasless fee quote (base units, §49.7 / §49.8 step 3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GaslessQuote {
    pub transfer_fee: U256,
    /// Charged only when the custody account is not yet on-chain activated.
    pub activation_fee: U256,
    /// `transfer_fee + activation_fee` (§49.8 step 3).
    pub total_token_fee: U256,
    pub nonce: u64,
    pub active: bool,
}

/// Why the gasless rail is disabled for a transfer (§49.7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DisabledReason {
    /// Safety stop (R2): the provider's custody address disagrees with the
    /// local `CREATE2` derivation. The wallet MUST NOT sign.
    AddressMismatch { local: String, provider: String },
    /// The token is not enrolled in the account.
    TokenUnsupported,
    /// The provider's token decimals disagree with the activated decimals.
    TokenDecimalMismatch { provider: u8, activated: u8 },
    /// Spendable (on-chain − frozen) is below `value + total fee`.
    InsufficientSpendableBalance { spendable: U256, required: U256 },
    /// Inactive account: on-chain balance below `value + total fee + frozen`.
    InactiveInsufficientBalance { balance: U256, required: U256 },
}

/// The outcome of a preflight (§49.7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreflightOutcome {
    /// The rail may sign (and, when wired, submit).
    Available(GaslessQuote),
    /// One-in-flight-per-account: wait for settlement and retry (transient).
    Pending,
    /// The rail cannot be used, for a classified reason.
    Disabled(DisabledReason),
}

/// Inputs gathered before the preflight decision (§49.7).
pub struct PreflightInputs<'a> {
    /// Locally-derived custody address (§49.5).
    pub derived_custody: &'a TronAddress,
    /// Provider account info (§49.4.4).
    pub account: &'a AccountInfo,
    /// On-chain TRC20 `balanceOf` of the custody address (base units).
    pub onchain_balance: U256,
    /// The token contract (EVM-form, matched against the account assets).
    pub token: &'a TronAddress,
    /// The activated token decimals.
    pub activated_decimals: u8,
    /// The transfer amount (base units).
    pub value: U256,
}

/// Decide whether the gasless rail can serve a transfer (§49.7).
pub fn preflight(inputs: &PreflightInputs) -> PreflightOutcome {
    // 1. Safety stop (R2): provider custody must match the local derivation.
    let local = inputs.derived_custody.to_base58();
    let provider_custody = match TronAddress::from_base58(inputs.account.gas_free_address.trim())
        .or_else(|_| TronAddress::from_hex(inputs.account.gas_free_address.trim()))
    {
        Ok(a) => a,
        Err(_) => {
            return PreflightOutcome::Disabled(DisabledReason::AddressMismatch {
                local,
                provider: inputs.account.gas_free_address.clone(),
            })
        },
    };
    if provider_custody != *inputs.derived_custody {
        return PreflightOutcome::Disabled(DisabledReason::AddressMismatch {
            local,
            provider: provider_custody.to_base58(),
        });
    }

    // 2. Token must be enrolled in the account.
    let token_evm = inputs.token.to_evm_address();
    let asset = inputs.account.assets.iter().find(|a| {
        TronAddress::from_base58(a.token_address.trim())
            .or_else(|_| TronAddress::from_hex(a.token_address.trim()))
            .map(|t| t.to_evm_address() == token_evm)
            .unwrap_or(false)
    });
    let asset = match asset {
        Some(a) => a,
        None => return PreflightOutcome::Disabled(DisabledReason::TokenUnsupported),
    };

    // 3. Decimals must agree (config/provider drift; never silently proceed).
    if asset.decimal != inputs.activated_decimals {
        return PreflightOutcome::Disabled(DisabledReason::TokenDecimalMismatch {
            provider: asset.decimal,
            activated: inputs.activated_decimals,
        });
    }

    // 4. One in-flight transfer per account → transient pending.
    if !inputs.account.allow_submit {
        return PreflightOutcome::Pending;
    }

    // 5. Fees: activation fee only for a not-yet-activated custody account.
    let transfer_fee = asset.transfer_fee;
    let activation_fee = if inputs.account.active {
        U256::zero()
    } else {
        asset.activate_fee
    };
    let total_token_fee = transfer_fee.saturating_add(activation_fee);
    let required = inputs.value.saturating_add(total_token_fee);

    // 6. Balance check, by activation state.
    if inputs.account.active {
        let spendable = inputs.onchain_balance.saturating_sub(asset.frozen);
        if spendable < required {
            return PreflightOutcome::Disabled(DisabledReason::InsufficientSpendableBalance { spendable, required });
        }
    } else {
        // Inactive: balance must cover value + fees + frozen.
        let required_inactive = required.saturating_add(asset.frozen);
        if inputs.onchain_balance < required_inactive {
            return PreflightOutcome::Disabled(DisabledReason::InactiveInsufficientBalance {
                balance: inputs.onchain_balance,
                required: required_inactive,
            });
        }
    }

    PreflightOutcome::Available(GaslessQuote {
        transfer_fee,
        activation_fee,
        total_token_fee,
        nonce: inputs.account.nonce,
        active: inputs.account.active,
    })
}

/// Resolve the effective fee cap (§49.8 step 3 / R8).
///
/// The effective cap is the minimum of the present caps (per-request,
/// activation-time); if neither is present, the quoted total fee itself is the
/// cap. If the quoted total exceeds the effective cap, the transfer is refused
/// (max-fee-exceeded). The returned value is the signed `maxFee`.
pub fn resolve_fee_cap(
    quoted_total_fee: U256,
    per_request_cap: Option<U256>,
    activation_cap: Option<U256>,
) -> Result<U256, GasFreeWithdrawError> {
    let effective = match (per_request_cap, activation_cap) {
        (Some(a), Some(b)) => a.min(b),
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => quoted_total_fee,
    };
    if quoted_total_fee > effective {
        return Err(GasFreeWithdrawError::FeeCapExceeded {
            fee: quoted_total_fee.to_string(),
            cap: effective.to_string(),
        });
    }
    Ok(effective)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eth::tron::gasfree::client::{AccountAsset, AccountInfo};
    use ethereum_types::Address as EthAddress;

    fn eth_from_u64(n: u64) -> EthAddress {
        let mut b = [0u8; 20];
        b[12..].copy_from_slice(&n.to_be_bytes());
        EthAddress::from(b)
    }

    fn tron(n: u64) -> TronAddress { TronAddress::from_evm_address(eth_from_u64(n)) }

    fn asset(token: &TronAddress, transfer_fee: u64, activate_fee: u64, decimal: u8, frozen: u64) -> AccountAsset {
        AccountAsset {
            token_address: token.to_base58(),
            token_symbol: "USDT".to_owned(),
            activate_fee: U256::from(activate_fee),
            transfer_fee: U256::from(transfer_fee),
            decimal,
            frozen: U256::from(frozen),
        }
    }

    fn account(custody: &TronAddress, active: bool, allow_submit: bool, assets: Vec<AccountAsset>) -> AccountInfo {
        AccountInfo {
            account_address: tron(1).to_base58(),
            gas_free_address: custody.to_base58(),
            active,
            nonce: 3,
            allow_submit,
            assets,
        }
    }

    fn inputs<'a>(
        custody: &'a TronAddress,
        account: &'a AccountInfo,
        token: &'a TronAddress,
        balance: u64,
        value: u64,
        decimals: u8,
    ) -> PreflightInputs<'a> {
        PreflightInputs {
            derived_custody: custody,
            account,
            onchain_balance: U256::from(balance),
            token,
            activated_decimals: decimals,
            value: U256::from(value),
        }
    }

    #[test]
    fn available_active_account() {
        let custody = tron(0x100);
        let token = tron(0x200);
        let acc = account(&custody, true, true, vec![asset(&token, 2, 5, 6, 0)]);
        let i = inputs(&custody, &acc, &token, 1_000, 100, 6);
        match preflight(&i) {
            PreflightOutcome::Available(q) => {
                assert_eq!(q.transfer_fee, U256::from(2u64));
                assert_eq!(q.activation_fee, U256::zero()); // active → no activation fee
                assert_eq!(q.total_token_fee, U256::from(2u64));
            },
            other => panic!("expected Available, got {other:?}"),
        }
    }

    #[test]
    fn available_inactive_includes_activation_fee() {
        let custody = tron(0x100);
        let token = tron(0x200);
        let acc = account(&custody, false, true, vec![asset(&token, 2, 5, 6, 0)]);
        let i = inputs(&custody, &acc, &token, 1_000, 100, 6);
        match preflight(&i) {
            PreflightOutcome::Available(q) => {
                assert_eq!(q.activation_fee, U256::from(5u64));
                assert_eq!(q.total_token_fee, U256::from(7u64));
            },
            other => panic!("expected Available, got {other:?}"),
        }
    }

    #[test]
    fn address_mismatch_is_safety_stop() {
        let custody = tron(0x100);
        let other = tron(0x999);
        let token = tron(0x200);
        let acc = account(&other, true, true, vec![asset(&token, 2, 5, 6, 0)]);
        let i = inputs(&custody, &acc, &token, 1_000, 100, 6);
        assert!(matches!(
            preflight(&i),
            PreflightOutcome::Disabled(DisabledReason::AddressMismatch { .. })
        ));
    }

    #[test]
    fn token_unsupported() {
        let custody = tron(0x100);
        let token = tron(0x200);
        let acc = account(&custody, true, true, vec![]);
        let i = inputs(&custody, &acc, &token, 1_000, 100, 6);
        assert!(matches!(
            preflight(&i),
            PreflightOutcome::Disabled(DisabledReason::TokenUnsupported)
        ));
    }

    #[test]
    fn token_decimal_mismatch() {
        let custody = tron(0x100);
        let token = tron(0x200);
        let acc = account(&custody, true, true, vec![asset(&token, 2, 5, 18, 0)]);
        let i = inputs(&custody, &acc, &token, 1_000, 100, 6);
        assert!(matches!(
            preflight(&i),
            PreflightOutcome::Disabled(DisabledReason::TokenDecimalMismatch {
                provider: 18,
                activated: 6
            })
        ));
    }

    #[test]
    fn pending_when_submit_disallowed() {
        let custody = tron(0x100);
        let token = tron(0x200);
        let acc = account(&custody, true, false, vec![asset(&token, 2, 5, 6, 0)]);
        let i = inputs(&custody, &acc, &token, 1_000, 100, 6);
        assert_eq!(preflight(&i), PreflightOutcome::Pending);
    }

    #[test]
    fn insufficient_spendable_balance_active() {
        let custody = tron(0x100);
        let token = tron(0x200);
        // frozen 900, balance 1000 → spendable 100; required = value(100)+fee(2)=102 > 100.
        let acc = account(&custody, true, true, vec![asset(&token, 2, 5, 6, 900)]);
        let i = inputs(&custody, &acc, &token, 1_000, 100, 6);
        assert!(matches!(
            preflight(&i),
            PreflightOutcome::Disabled(DisabledReason::InsufficientSpendableBalance { .. })
        ));
    }

    #[test]
    fn inactive_insufficient_balance() {
        let custody = tron(0x100);
        let token = tron(0x200);
        // inactive: required = value(100)+transfer(2)+activate(5)+frozen(50)=157 > balance 150.
        let acc = account(&custody, false, true, vec![asset(&token, 2, 5, 6, 50)]);
        let i = inputs(&custody, &acc, &token, 150, 100, 6);
        assert!(matches!(
            preflight(&i),
            PreflightOutcome::Disabled(DisabledReason::InactiveInsufficientBalance { .. })
        ));
    }

    #[test]
    fn fee_cap_min_of_present_caps() {
        let quoted = U256::from(10u64);
        // both present → min(8, 12) = 8; quoted 10 > 8 → refuse.
        assert!(resolve_fee_cap(quoted, Some(U256::from(8u64)), Some(U256::from(12u64))).is_err());
        // both present → min(15, 12) = 12; quoted 10 <= 12 → ok, maxFee = 12.
        assert_eq!(
            resolve_fee_cap(quoted, Some(U256::from(15u64)), Some(U256::from(12u64))).unwrap(),
            U256::from(12u64)
        );
    }

    #[test]
    fn fee_cap_single_present() {
        let quoted = U256::from(10u64);
        assert_eq!(
            resolve_fee_cap(quoted, Some(U256::from(20u64)), None).unwrap(),
            U256::from(20u64)
        );
        assert!(resolve_fee_cap(quoted, Some(U256::from(5u64)), None).is_err());
    }

    #[test]
    fn fee_cap_none_present_uses_quoted_total() {
        let quoted = U256::from(10u64);
        assert_eq!(resolve_fee_cap(quoted, None, None).unwrap(), quoted);
    }
}
