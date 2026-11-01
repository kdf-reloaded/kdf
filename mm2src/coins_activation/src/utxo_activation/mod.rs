mod common_impl;
mod init_bch_activation;
mod init_qtum_activation;
mod init_utxo_standard_activation;
mod init_utxo_standard_activation_error;
mod init_utxo_standard_statuses;
mod utxo_standard_activation_result;

pub use init_bch_activation::BchTaskManagerShared;
pub use init_qtum_activation::QtumTaskManagerShared;
pub use init_utxo_standard_activation::UtxoStandardTaskManagerShared;

#[cfg(test)] mod tests;
