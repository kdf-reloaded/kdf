//! App identity advertised to wallets during session proposal.

use relay_rpc::rpc::params::Metadata;

/// The default relay address the subsystem connects to.
pub const RELAY_ADDRESS: &str = "wss://relay.walletconnect.com";

/// The relay transport protocol we negotiate (IRN).
pub const SUPPORTED_RELAY_PROTOCOL: &str = "irn";

/// Prefix of the application name presented to the wallet; the client key fingerprint follows it.
pub const APP_NAME_PREFIX: &str = "KDF Reloaded";

/// Application description presented to the wallet.
pub const APP_DESCRIPTION: &str = "KDF Reloaded WalletConnect client";

/// Canonical application URL.
pub const APP_URL: &str = "https://komodoplatform.com";

/// The application name presented to the wallet: `operator_name` when it is present and not empty, else
/// `"KDF Reloaded <fingerprint>"`, where `fingerprint` is the short fingerprint of the relay client key.
pub fn app_name(operator_name: Option<&str>, fingerprint: &str) -> String {
    match operator_name.filter(|name| !name.is_empty()) {
        Some(name) => name.to_string(),
        None => format!("{APP_NAME_PREFIX} {fingerprint}"),
    }
}

/// Builds the [`Metadata`] block advertised in a session proposal.
pub fn generate_metadata(operator_name: Option<&str>, fingerprint: &str) -> Metadata {
    Metadata {
        name: app_name(operator_name, fingerprint),
        description: APP_DESCRIPTION.to_string(),
        url: APP_URL.to_string(),
        icons: Vec::new(),
    }
}
