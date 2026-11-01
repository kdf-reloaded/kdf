use crate::prelude::CurrentBlock;
use coins::coin_balance::{coin_balance_map_for_ticker, CoinBalanceMap, EnableCoinBalance, HDWalletBalance};
use coins::CoinBalance;
use serde_derive::Serialize;

/// The success object of the UTXO-family members (CRD ch. 55 R55.5.13).
#[derive(Clone, Serialize)]
pub struct UtxoStandardActivationResult {
    pub ticker: String,
    pub current_block: u64,
    pub wallet_balance: UtxoWalletBalance,
}

/// The wallet balance of a UTXO-family activation. Both forms key their balances by the activated ticker.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "wallet_type")]
pub enum UtxoWalletBalance {
    Iguana(UtxoIguanaWalletBalance),
    HD(HDWalletBalance),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UtxoIguanaWalletBalance {
    pub address: String,
    pub balance: CoinBalanceMap,
}

impl UtxoWalletBalance {
    pub fn iguana(ticker: &str, address: String, balance: CoinBalance) -> Self {
        UtxoWalletBalance::Iguana(UtxoIguanaWalletBalance {
            address,
            balance: coin_balance_map_for_ticker(ticker, balance),
        })
    }

    pub fn from_enable_coin_balance(ticker: &str, balance: EnableCoinBalance) -> Self {
        match balance {
            EnableCoinBalance::Iguana(iguana) => Self::iguana(ticker, iguana.address, iguana.balance),
            EnableCoinBalance::HD(hd) => UtxoWalletBalance::HD(hd),
        }
    }
}

impl CurrentBlock for UtxoStandardActivationResult {
    fn current_block(&self) -> u64 { self.current_block }
}
