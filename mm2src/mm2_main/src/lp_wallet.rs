/// Wallet management: encrypted mnemonic persistence, wallet lifecycle RPCs.
///
/// Wallets are stored as JSON files named `<wallet_name>.json` directly in the
/// database root directory (the parent of the per-identity hex subdirectories,
/// Chapter 07 §7.5), each containing the canonical six-field `EncryptedMnemonicData`
/// envelope (§7.7). The wallet password is verified by attempting to decrypt the
/// mnemonic — no password hash is stored on disk.
///
/// For backward compatibility the read paths also recognise wallets written by
/// the previous reloaded build at `<db_root>/wallets/<name>.wallet` (the legacy
/// two-field envelope). New writes are ALWAYS the canonical `.json` format.
///
/// The currently active wallet name is recorded in `MmCtx::wallet_name` (write-once)
/// during startup. Only inactive wallets can be deleted.
use common::HttpStatusCode;
use crypto::{decrypt_mnemonic, encrypt_mnemonic, generate_mnemonic};
use derive_more::Display;
use http::StatusCode;
use mm2_core::mm_ctx::MmArc;
use mm2_err_handle::prelude::*;
use ser_error_derive::SerializeErrorType;
use serde::{Deserialize, Serialize};

#[cfg(not(target_arch = "wasm32"))]
mod storage {
    use crypto::legacy::{legacy_decrypt_mnemonic, LegacyEncryptedMnemonicData};
    use crypto::{decrypt_mnemonic, EncryptedMnemonicData};
    use mm2_core::mm_ctx::MmArc;
    use mm2_io::fs::{read_dir_async, remove_file_async};
    use std::io;
    use std::path::PathBuf;

    /// The project-wide on-disk wallet-file extension (§7.5 R8).
    pub const WALLET_FILE_EXTENSION: &str = "json";

    /// The legacy on-disk wallet-file extension written by an earlier reloaded
    /// build. Recognised on read only.
    const LEGACY_WALLET_FILE_EXTENSION: &str = "wallet";

    /// A persisted wallet record, in either the canonical §7.7 `.json` format or
    /// the legacy `wallets/*.wallet` format (read-only).
    pub enum StoredEnvelope {
        New(EncryptedMnemonicData),
        Legacy(LegacyEncryptedMnemonicData),
    }

    impl StoredEnvelope {
        /// Decrypts the stored mnemonic with `password`, regardless of format.
        pub fn decrypt(&self, password: &str) -> Result<String, String> {
            match self {
                StoredEnvelope::New(e) => decrypt_mnemonic(e, password).map_err(|e| e.to_string()),
                StoredEnvelope::Legacy(e) => legacy_decrypt_mnemonic(e, password).map_err(|e| e.to_string()),
            }
        }

        /// Serializes the stored envelope verbatim for the encrypted `get_mnemonic`
        /// export form (§7.3A R-K7): the stored record is returned unchanged.
        pub fn as_json(&self) -> serde_json::Value {
            match self {
                StoredEnvelope::New(e) => serde_json::to_value(e).unwrap_or(serde_json::Value::Null),
                StoredEnvelope::Legacy(e) => serde_json::to_value(e).unwrap_or(serde_json::Value::Null),
            }
        }
    }

    /// Returns the database root directory, creating it if needed (§7.5 R7).
    fn ensure_db_root(ctx: &MmArc) -> io::Result<PathBuf> {
        let dir = ctx.db_root();
        if !dir.exists() {
            std::fs::create_dir_all(&dir)?;
        }
        Ok(dir)
    }

    /// Path to a canonical wallet file `<db_root>/<wallet_name>.json`.
    fn wallet_path(ctx: &MmArc, wallet_name: &str) -> io::Result<PathBuf> {
        Ok(ensure_db_root(ctx)?.join(format!("{}.{}", wallet_name, WALLET_FILE_EXTENSION)))
    }

    /// Path to a legacy wallet file `<db_root>/wallets/<wallet_name>.wallet`.
    fn legacy_wallet_path(ctx: &MmArc, wallet_name: &str) -> PathBuf {
        ctx.wallets_dir()
            .join(format!("{}.{}", wallet_name, LEGACY_WALLET_FILE_EXTENSION))
    }

    /// Save the encrypted mnemonic for a wallet as the canonical `.json` record in
    /// the database root. Overwrites if the wallet already exists. New writes are
    /// NEVER the legacy `.wallet` format.
    pub async fn save_encrypted_passphrase(
        ctx: &MmArc,
        wallet_name: &str,
        data: &EncryptedMnemonicData,
    ) -> Result<(), String> {
        let path = wallet_path(ctx, wallet_name).map_err(|e| format!("wallet dir error: {e}"))?;
        let json = serde_json::to_string_pretty(data).map_err(|e| format!("serialize error: {e}"))?;
        mm2_io::fs::write(&path, &json.as_bytes()).map_err(|e| format!("write error: {e}"))
    }

    /// Read the encrypted mnemonic for a wallet. Tries the canonical `.json`
    /// record first, then falls back to the legacy `wallets/<name>.wallet` record.
    /// Returns `None` if neither exists.
    pub async fn read_encrypted_passphrase(ctx: &MmArc, wallet_name: &str) -> Result<Option<StoredEnvelope>, String> {
        // Canonical §7.7 record in the database root.
        let path = wallet_path(ctx, wallet_name).map_err(|e| format!("wallet dir error: {e}"))?;
        if path.exists() {
            let bytes = mm2_io::fs::slurp(&path)?;
            let data: EncryptedMnemonicData =
                serde_json::from_slice(&bytes).map_err(|e| format!("corrupt wallet file: {e}"))?;
            return Ok(Some(StoredEnvelope::New(data)));
        }

        // Legacy fallback: `<db_root>/wallets/<name>.wallet` (read-only).
        let legacy_path = legacy_wallet_path(ctx, wallet_name);
        if legacy_path.exists() {
            let bytes = mm2_io::fs::slurp(&legacy_path)?;
            let data: LegacyEncryptedMnemonicData =
                serde_json::from_slice(&bytes).map_err(|e| format!("corrupt legacy wallet file: {e}"))?;
            return Ok(Some(StoredEnvelope::Legacy(data)));
        }

        Ok(None)
    }

    /// Returns the trimmed file stem if `path` bears `extension` and the trimmed
    /// stem matches the bound wallet-name grammar (§7.5 R8A), else `None`.
    fn wallet_stem_for_extension(path: &std::path::Path, extension: &str) -> Option<String> {
        if path.extension().and_then(|e| e.to_str()) != Some(extension) {
            return None;
        }
        let stem = path.file_stem()?.to_str()?.trim().to_string();
        if super::validate_wallet_name(&stem).is_ok() {
            Some(stem)
        } else {
            None
        }
    }

    /// List all wallet names. Scans the database root **non-recursively** for
    /// canonical `.json` records (§7.5 R8A), plus any legacy `wallets/*.wallet`
    /// records, and de-duplicates (preferring the canonical record).
    pub async fn read_all_wallet_names(ctx: &MmArc) -> Result<Vec<String>, String> {
        let root = ensure_db_root(ctx).map_err(|e| format!("wallet dir error: {e}"))?;
        // Non-recursive: `read_dir_async` lists immediate children only; filtering
        // by the `.json` extension naturally excludes the per-identity subdirectories.
        let root_entries = read_dir_async(&root)
            .await
            .map_err(|e| format!("read dir error: {e}"))?;
        let mut names: Vec<String> = root_entries
            .iter()
            .filter_map(|p| wallet_stem_for_extension(p, WALLET_FILE_EXTENSION))
            .collect();

        // Legacy directory (may not exist).
        let legacy_dir = ctx.wallets_dir();
        if legacy_dir.exists() {
            if let Ok(legacy_entries) = read_dir_async(&legacy_dir).await {
                for p in &legacy_entries {
                    if let Some(stem) = wallet_stem_for_extension(p, LEGACY_WALLET_FILE_EXTENSION) {
                        if !names.contains(&stem) {
                            names.push(stem);
                        }
                    }
                }
            }
        }

        Ok(names)
    }

    /// Delete a wallet record from whichever location it exists in (canonical
    /// `.json` in the root, or legacy `wallets/<name>.wallet`).
    pub async fn delete_wallet(ctx: &MmArc, wallet_name: &str) -> Result<(), String> {
        let path = wallet_path(ctx, wallet_name).map_err(|e| format!("wallet dir error: {e}"))?;
        if path.exists() {
            return remove_file_async(path).await.map_err(|e| format!("delete error: {e}"));
        }
        let legacy_path = legacy_wallet_path(ctx, wallet_name);
        if legacy_path.exists() {
            return remove_file_async(legacy_path)
                .await
                .map_err(|e| format!("delete error: {e}"));
        }
        Err(format!("Wallet '{}' not found", wallet_name))
    }
}

#[cfg(not(target_arch = "wasm32"))]
use storage::{delete_wallet, read_all_wallet_names, read_encrypted_passphrase, save_encrypted_passphrase};

// --- Error types ---

/// Errors for wallet initialization and wallet management RPCs.
#[derive(Clone, Debug, Display, Serialize, SerializeErrorType)]
#[serde(tag = "error_type", content = "error_data")]
pub enum WalletError {
    #[display(fmt = "Invalid request: {}", _0)]
    InvalidRequest(String),
    #[display(fmt = "Invalid password")]
    InvalidPassword,
    #[display(fmt = "Wallet '{}' already exists", _0)]
    WalletAlreadyExists(String),
    #[display(fmt = "Wallet '{}' not found", _0)]
    WalletNotFound(String),
    #[display(fmt = "Cannot delete active wallet '{}'", _0)]
    CannotDeleteActiveWallet(String),
    #[display(fmt = "Storage error: {}", _0)]
    StorageError(String),
    #[display(fmt = "Encryption error: {}", _0)]
    EncryptionError(String),
    #[display(fmt = "Internal error: {}", _0)]
    Internal(String),
}

impl HttpStatusCode for WalletError {
    fn status_code(&self) -> StatusCode {
        match self {
            WalletError::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            WalletError::InvalidPassword => StatusCode::BAD_REQUEST,
            WalletError::WalletAlreadyExists(_) => StatusCode::CONFLICT,
            WalletError::WalletNotFound(_) => StatusCode::NOT_FOUND,
            WalletError::CannotDeleteActiveWallet(_) => StatusCode::BAD_REQUEST,
            WalletError::StorageError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            WalletError::EncryptionError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            WalletError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

// --- Wallet name validation ---

/// Validates wallet name: alphanumeric, dash, underscore, space. 1-64 chars.
fn validate_wallet_name(name: &str) -> Result<(), WalletError> {
    if name.is_empty() || name.len() > 64 {
        return Err(WalletError::InvalidRequest(
            "Wallet name must be 1-64 characters".to_string(),
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == ' ')
    {
        return Err(WalletError::InvalidRequest(
            "Wallet name may only contain alphanumeric characters, dashes, underscores, and spaces".to_string(),
        ));
    }
    Ok(())
}

// --- RPC request/response types ---

#[derive(Deserialize)]
pub struct CreateWalletRequest {
    pub wallet_name: String,
    pub password: String,
    pub mnemonic: String,
}

#[derive(Debug, Serialize)]
pub struct CreateWalletResponse {
    pub wallet_name: String,
}

#[derive(Deserialize)]
pub struct GetWalletNamesRequest {}

#[derive(Debug, Serialize)]
pub struct GetWalletNamesResponse {
    pub wallet_names: Vec<String>,
    /// The currently active wallet, or `null` when not logged in.
    ///
    /// The wire field name is `activated_wallet` to match the upstream
    /// `get_wallet_names` contract that clients (e.g. the Komodo DeFi SDK) rely
    /// on to determine the signed-in wallet.
    pub activated_wallet: Option<String>,
}

#[derive(Deserialize)]
pub struct DeleteWalletRequest {
    pub wallet_name: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct DeleteWalletResponse {
    pub wallet_name: String,
}

/// Selects how the caller's own seed is returned by `get_mnemonic`.
///
/// `encrypted` returns the stored encryption envelope unchanged (no password,
/// no plaintext exposure); `plaintext` requires the wallet password and returns
/// the decoded mnemonic string.
#[derive(Deserialize)]
#[serde(tag = "format", rename_all = "lowercase")]
pub enum GetMnemonicRequest {
    Encrypted,
    Plaintext { password: String },
}

/// Mirrors the requested `format`: the encrypted form carries the stored
/// envelope verbatim (in whichever on-disk format it was persisted), the
/// plaintext form carries the decoded mnemonic string.
#[derive(Serialize)]
#[serde(tag = "format", rename_all = "lowercase")]
pub enum GetMnemonicResponse {
    Encrypted { encrypted_mnemonic_data: serde_json::Value },
    Plaintext { mnemonic: String },
}

/// Changes the wallet password by re-encrypting the stored mnemonic.
///
/// `current_password` authenticates the request (it must decrypt the stored
/// envelope); `new_password` becomes the wallet password going forward.
#[derive(Deserialize)]
pub struct ChangeMnemonicPasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

// --- RPC handlers ---

/// Creates a new wallet by encrypting and persisting the given mnemonic.
///
/// The wallet must not already exist. The mnemonic is validated as BIP39 by the
/// encryption layer. After this call, the wallet can be used in future sessions
/// by providing `wallet_name` + `password` at startup.
#[cfg(not(target_arch = "wasm32"))]
pub async fn create_wallet_rpc(
    ctx: MmArc,
    req: CreateWalletRequest,
) -> Result<CreateWalletResponse, MmError<WalletError>> {
    validate_wallet_name(&req.wallet_name)?;

    if req.password.is_empty() {
        return MmError::err(WalletError::InvalidRequest("Password cannot be empty".to_string()));
    }

    // Check if wallet already exists
    let existing = read_encrypted_passphrase(&ctx, &req.wallet_name)
        .await
        .map_err(|e| MmError::new(WalletError::StorageError(e)))?;
    if existing.is_some() {
        return MmError::err(WalletError::WalletAlreadyExists(req.wallet_name));
    }

    // Encrypt the mnemonic with the password
    let encrypted = encrypt_mnemonic(&req.mnemonic, &req.password)
        .map_err(|e| MmError::new(WalletError::EncryptionError(e.to_string())))?;

    // Persist the encrypted mnemonic
    save_encrypted_passphrase(&ctx, &req.wallet_name, &encrypted)
        .await
        .map_err(|e| MmError::new(WalletError::StorageError(e)))?;

    Ok(CreateWalletResponse {
        wallet_name: req.wallet_name,
    })
}

/// Lists all wallet names and identifies the currently active wallet.
#[cfg(not(target_arch = "wasm32"))]
pub async fn get_wallet_names_rpc(
    ctx: MmArc,
    _req: GetWalletNamesRequest,
) -> Result<GetWalletNamesResponse, MmError<WalletError>> {
    let wallet_names = read_all_wallet_names(&ctx)
        .await
        .map_err(|e| MmError::new(WalletError::StorageError(e)))?;

    let activated_wallet = ctx.wallet_name.as_option().and_then(|opt| opt.clone());

    Ok(GetWalletNamesResponse {
        wallet_names,
        activated_wallet,
    })
}

/// Deletes an inactive wallet after verifying the password.
///
/// The password is verified by decrypting the stored mnemonic. If decryption
/// succeeds, the wallet file is removed. The currently active wallet cannot
/// be deleted — stop the node first.
#[cfg(not(target_arch = "wasm32"))]
pub async fn delete_wallet_rpc(
    ctx: MmArc,
    req: DeleteWalletRequest,
) -> Result<DeleteWalletResponse, MmError<WalletError>> {
    validate_wallet_name(&req.wallet_name)?;

    // Block deletion of the active wallet
    if let Some(Some(active)) = ctx.wallet_name.as_option() {
        if active == &req.wallet_name {
            return MmError::err(WalletError::CannotDeleteActiveWallet(req.wallet_name));
        }
    }

    // Load the encrypted mnemonic
    let encrypted = read_encrypted_passphrase(&ctx, &req.wallet_name)
        .await
        .map_err(|e| MmError::new(WalletError::StorageError(e)))?
        .ok_or_else(|| MmError::new(WalletError::WalletNotFound(req.wallet_name.clone())))?;

    // Verify password by attempting decryption
    encrypted
        .decrypt(&req.password)
        .map_err(|_| MmError::new(WalletError::InvalidPassword))?;

    // Password verified — delete the wallet file
    delete_wallet(&ctx, &req.wallet_name)
        .await
        .map_err(|e| MmError::new(WalletError::StorageError(e)))?;

    Ok(DeleteWalletResponse {
        wallet_name: req.wallet_name,
    })
}

/// Returns the caller's *own* seed, authenticated by the wallet password.
///
/// `encrypted` format returns the stored envelope unchanged (no password
/// required, no decryption). `plaintext` format decrypts the stored mnemonic
/// under the supplied wallet password; a wrong password yields `InvalidPassword`
/// and never a usable-but-corrupt plaintext. Host-side seed export is refused
/// for hardware-wallet (Trezor) sessions. The recovered secret is serialized
/// exactly once, for the response only, and is never logged or persisted.
#[cfg(not(target_arch = "wasm32"))]
pub async fn get_mnemonic_rpc(
    ctx: MmArc,
    req: GetMnemonicRequest,
) -> Result<GetMnemonicResponse, MmError<WalletError>> {
    // Reject hardware-wallet (Trezor) sessions: host-side seed export is never
    // performed for hardware wallets.
    let crypto_ctx =
        crypto::CryptoCtx::from_ctx(&ctx).map_err(|e| MmError::new(WalletError::Internal(e.to_string())))?;
    if crypto_ctx.hw_ctx().is_some() {
        return MmError::err(WalletError::InvalidRequest(
            "Mnemonic export is not supported for hardware-wallet sessions".to_string(),
        ));
    }

    // Resolve the active wallet name.
    let wallet_name = match ctx.wallet_name.as_option() {
        Some(Some(name)) => name.clone(),
        _ => return MmError::err(WalletError::InvalidRequest("No active wallet".to_string())),
    };

    // Load the stored encryption envelope.
    let encrypted = read_encrypted_passphrase(&ctx, &wallet_name)
        .await
        .map_err(|e| MmError::new(WalletError::StorageError(e)))?
        .ok_or_else(|| MmError::new(WalletError::WalletNotFound(wallet_name.clone())))?;

    match req {
        GetMnemonicRequest::Encrypted => Ok(GetMnemonicResponse::Encrypted {
            encrypted_mnemonic_data: encrypted.as_json(),
        }),
        GetMnemonicRequest::Plaintext { password } => {
            let mnemonic = encrypted
                .decrypt(&password)
                .map_err(|_| MmError::new(WalletError::InvalidPassword))?;
            Ok(GetMnemonicResponse::Plaintext { mnemonic })
        },
    }
}

/// Re-encrypts the caller's *own* stored mnemonic under a new wallet password.
///
/// `current_password` is verified by decrypting the stored envelope; a wrong
/// password yields `InvalidPassword`. On success the mnemonic is re-wrapped
/// under `new_password` and the wallet record is overwritten in place. Refused
/// for hardware-wallet (Trezor) sessions, which hold no host-side mnemonic. The
/// recovered secret lives only for the duration of the re-encryption and is
/// never logged or persisted in plaintext.
#[cfg(not(target_arch = "wasm32"))]
pub async fn change_mnemonic_password_rpc(
    ctx: MmArc,
    req: ChangeMnemonicPasswordRequest,
) -> Result<(), MmError<WalletError>> {
    // Reject hardware-wallet (Trezor) sessions: there is no host-side mnemonic to re-encrypt.
    let crypto_ctx =
        crypto::CryptoCtx::from_ctx(&ctx).map_err(|e| MmError::new(WalletError::Internal(e.to_string())))?;
    if crypto_ctx.hw_ctx().is_some() {
        return MmError::err(WalletError::InvalidRequest(
            "Changing the mnemonic password is not supported for hardware-wallet sessions".to_string(),
        ));
    }

    if req.new_password.is_empty() {
        return MmError::err(WalletError::InvalidRequest("new_password cannot be empty".to_string()));
    }

    // Resolve the active wallet name.
    let wallet_name = match ctx.wallet_name.as_option() {
        Some(Some(name)) => name.clone(),
        _ => return MmError::err(WalletError::InvalidRequest("No active wallet".to_string())),
    };

    // Load the stored encryption envelope.
    let encrypted = read_encrypted_passphrase(&ctx, &wallet_name)
        .await
        .map_err(|e| MmError::new(WalletError::StorageError(e)))?
        .ok_or_else(|| MmError::new(WalletError::WalletNotFound(wallet_name.clone())))?;

    // Verify the current password by decrypting the stored mnemonic.
    let mnemonic = encrypted
        .decrypt(&req.current_password)
        .map_err(|_| MmError::new(WalletError::InvalidPassword))?;

    // Re-encrypt the mnemonic under the new password and overwrite the record.
    let re_encrypted = encrypt_mnemonic(&mnemonic, &req.new_password)
        .map_err(|e| MmError::new(WalletError::EncryptionError(e.to_string())))?;

    save_encrypted_passphrase(&ctx, &wallet_name, &re_encrypted)
        .await
        .map_err(|e| MmError::new(WalletError::StorageError(e)))?;

    Ok(())
}

// --- Startup integration ---

use super::lp_native_dex::PassphraseForm;

/// Word count for a freshly generated wallet mnemonic.
#[cfg(not(target_arch = "wasm32"))]
const GENERATED_MNEMONIC_WORD_COUNT: usize = 24;

/// Validates `wallet_password` for non-emptiness and, unless `allow_weak_password`
/// is set, against the password policy. Used only on the rows that create or
/// first-save a record (R29).
#[cfg(not(target_arch = "wasm32"))]
fn enforce_wallet_password_policy(ctx: &MmArc, password: &str) -> Result<(), MmError<WalletError>> {
    if password.is_empty() {
        return MmError::err(WalletError::InvalidRequest(
            "wallet_password cannot be empty".to_string(),
        ));
    }
    let allow_weak = ctx.conf["allow_weak_password"].as_bool() == Some(true);
    if !allow_weak {
        super::password_policy(password).map_err(|e| MmError::new(WalletError::InvalidRequest(e.to_string())))?;
    }
    Ok(())
}

/// Encrypts and persists a plaintext mnemonic as `<wallet_name>` record.
#[cfg(not(target_arch = "wasm32"))]
async fn persist_passphrase(
    ctx: &MmArc,
    wallet_name: &str,
    mnemonic: &str,
    password: &str,
) -> Result<(), MmError<WalletError>> {
    let encrypted =
        encrypt_mnemonic(mnemonic, password).map_err(|e| MmError::new(WalletError::EncryptionError(e.to_string())))?;
    save_encrypted_passphrase(ctx, wallet_name, &encrypted)
        .await
        .map_err(|e| MmError::new(WalletError::StorageError(e)))
}

/// Resolves the signing-identity seed at startup from the three configuration
/// inputs of R18, applying the full R27 decision matrix. The returned `Some(seed)`
/// is the **plaintext** mnemonic the caller must initialise the signing identity
/// with (R28); `None` is the anonymous row (no identity). The active-wallet slot
/// is pinned as a side effect.
///
/// Failure modes are kept distinct (R29): a wrong `wallet_password` surfaces as
/// `InvalidPassword` (decryption failure); a correctly-decrypting but conflicting
/// stored seed surfaces as `InvalidRequest` (passphrase mismatch). Every refusal
/// returns an error so startup fails closed before serving (R30).
#[cfg(not(target_arch = "wasm32"))]
pub async fn initialize_wallet_passphrase(
    ctx: &MmArc,
    passphrase: PassphraseForm,
    wallet_name: Option<&str>,
    wallet_password: Option<&str>,
) -> Result<Option<String>, MmError<WalletError>> {
    // `wallet_name` absent: anonymous, legacy-plaintext, or refuse-encrypted rows.
    let name = match wallet_name {
        None => {
            let _ = ctx.wallet_name.pin(None);
            return match passphrase {
                PassphraseForm::Absent => Ok(None),
                PassphraseForm::Plaintext(seed) => Ok(Some(seed)),
                PassphraseForm::Encrypted(_) => MmError::err(WalletError::InvalidRequest(
                    "wallet_name is required to use an encrypted passphrase".to_string(),
                )),
            };
        },
        Some(name) => name,
    };

    // `wallet_name` present: `wallet_password` is mandatory.
    let password = match wallet_password {
        Some(password) => password,
        None => {
            return MmError::err(WalletError::InvalidRequest(
                "wallet_password is required when wallet_name is set".to_string(),
            ));
        },
    };

    validate_wallet_name(name)?;

    let existing = read_encrypted_passphrase(ctx, name)
        .await
        .map_err(|e| MmError::new(WalletError::StorageError(e)))?;

    let seed = match (passphrase, existing) {
        // Re-login: load-and-use, with NO equality comparison (the core re-login fix).
        (PassphraseForm::Absent, Some(encrypted)) => encrypted
            .decrypt(password)
            .map_err(|_| MmError::new(WalletError::InvalidPassword))?,
        // Generate-and-persist a fresh mnemonic.
        (PassphraseForm::Absent, None) => {
            enforce_wallet_password_policy(ctx, password)?;
            let mnemonic = generate_mnemonic(GENERATED_MNEMONIC_WORD_COUNT)
                .map_err(|e| MmError::new(WalletError::Internal(e.to_string())))?
                .to_string();
            persist_passphrase(ctx, name, &mnemonic, password).await?;
            mnemonic
        },
        // First-save of the supplied plaintext seed.
        (PassphraseForm::Plaintext(seed), None) => {
            enforce_wallet_password_policy(ctx, password)?;
            persist_passphrase(ctx, name, &seed, password).await?;
            seed
        },
        // Confirm the supplied plaintext seed against the stored record.
        (PassphraseForm::Plaintext(seed), Some(encrypted)) => {
            let stored = encrypted
                .decrypt(password)
                .map_err(|_| MmError::new(WalletError::InvalidPassword))?;
            if stored != seed {
                return MmError::err(WalletError::InvalidRequest(
                    "Passphrase doesn't match the stored wallet. Create a new wallet to use a different passphrase"
                        .to_string(),
                ));
            }
            stored
        },
        // Import-and-save the supplied envelope verbatim.
        (PassphraseForm::Encrypted(supplied), None) => {
            enforce_wallet_password_policy(ctx, password)?;
            let seed = decrypt_mnemonic(&supplied, password).map_err(|_| MmError::new(WalletError::InvalidPassword))?;
            save_encrypted_passphrase(ctx, name, &supplied)
                .await
                .map_err(|e| MmError::new(WalletError::StorageError(e)))?;
            seed
        },
        // Confirm the supplied envelope against the stored record.
        (PassphraseForm::Encrypted(supplied), Some(stored)) => {
            let supplied_seed =
                decrypt_mnemonic(&supplied, password).map_err(|_| MmError::new(WalletError::InvalidPassword))?;
            let stored_seed = stored
                .decrypt(password)
                .map_err(|_| MmError::new(WalletError::InvalidPassword))?;
            if supplied_seed != stored_seed {
                return MmError::err(WalletError::InvalidRequest(
                    "Passphrase doesn't match the stored wallet. Create a new wallet to use a different passphrase"
                        .to_string(),
                ));
            }
            stored_seed
        },
    };

    let _ = ctx.wallet_name.pin(Some(name.to_string()));
    Ok(Some(seed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::block_on;
    use http::StatusCode;
    use mm2_core::mm_ctx::MmCtxBuilder;
    use serde_json::json;
    use std::env;

    /// Create a test MmCtx with a unique temp dbdir.
    fn test_ctx() -> MmArc {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = env::temp_dir().join(format!("kdf_wallet_test_{}_{}", common::now_ms(), unique));
        MmCtxBuilder::default()
            .with_conf(json!({"dbdir": dir.to_str().unwrap(), "allow_weak_password": true}))
            .into_mm_arc()
    }

    #[test]
    fn test_validate_wallet_name() {
        assert!(validate_wallet_name("my-wallet_1").is_ok());
        assert!(validate_wallet_name("My Wallet").is_ok());
        assert!(validate_wallet_name("a").is_ok());
        assert!(validate_wallet_name(&"x".repeat(64)).is_ok());

        // Too long
        assert!(validate_wallet_name(&"x".repeat(65)).is_err());
        // Empty
        assert!(validate_wallet_name("").is_err());
        // Invalid chars
        assert!(validate_wallet_name("wallet/bad").is_err());
        assert!(validate_wallet_name("wallet..bad").is_err());
        assert!(validate_wallet_name("wallet\0").is_err());
    }

    #[test]
    fn test_wallet_error_status_codes() {
        assert_eq!(
            WalletError::InvalidRequest("x".into()).status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(WalletError::InvalidPassword.status_code(), StatusCode::BAD_REQUEST);
        assert_eq!(
            WalletError::WalletAlreadyExists("x".into()).status_code(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            WalletError::WalletNotFound("x".into()).status_code(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            WalletError::CannotDeleteActiveWallet("x".into()).status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            WalletError::StorageError("x".into()).status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn test_wallet_lifecycle_create_list_delete() {
        let ctx = test_ctx();
        let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let password = "test_password_123";

        // Create wallet
        let resp = block_on(create_wallet_rpc(ctx.clone(), CreateWalletRequest {
            wallet_name: "test-wallet".to_string(),
            password: password.to_string(),
            mnemonic: mnemonic.to_string(),
        }))
        .unwrap();
        assert_eq!(resp.wallet_name, "test-wallet");

        // List wallets — should contain exactly one
        let list = block_on(get_wallet_names_rpc(ctx.clone(), GetWalletNamesRequest {})).unwrap();
        assert_eq!(list.wallet_names, vec!["test-wallet".to_string()]);
        assert_eq!(list.activated_wallet, None); // no active wallet set

        // Delete with wrong password — should fail
        let err = block_on(delete_wallet_rpc(ctx.clone(), DeleteWalletRequest {
            wallet_name: "test-wallet".to_string(),
            password: "wrong_password".to_string(),
        }))
        .unwrap_err();
        assert_eq!(err.get_inner().status_code(), StatusCode::BAD_REQUEST);

        // Delete with correct password
        let resp = block_on(delete_wallet_rpc(ctx.clone(), DeleteWalletRequest {
            wallet_name: "test-wallet".to_string(),
            password: password.to_string(),
        }))
        .unwrap();
        assert_eq!(resp.wallet_name, "test-wallet");

        // List wallets — should be empty now
        let list = block_on(get_wallet_names_rpc(ctx.clone(), GetWalletNamesRequest {})).unwrap();
        assert!(list.wallet_names.is_empty());
    }

    #[test]
    fn test_create_duplicate_wallet_fails() {
        let ctx = test_ctx();
        let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let password = "pw123";

        block_on(create_wallet_rpc(ctx.clone(), CreateWalletRequest {
            wallet_name: "dup-wallet".to_string(),
            password: password.to_string(),
            mnemonic: mnemonic.to_string(),
        }))
        .unwrap();

        // Second create should fail with Conflict
        let err = block_on(create_wallet_rpc(ctx.clone(), CreateWalletRequest {
            wallet_name: "dup-wallet".to_string(),
            password: password.to_string(),
            mnemonic: mnemonic.to_string(),
        }))
        .unwrap_err();
        assert_eq!(err.get_inner().status_code(), StatusCode::CONFLICT);
    }

    #[test]
    fn test_delete_nonexistent_wallet_fails() {
        let ctx = test_ctx();

        let err = block_on(delete_wallet_rpc(ctx.clone(), DeleteWalletRequest {
            wallet_name: "ghost-wallet".to_string(),
            password: "any".to_string(),
        }))
        .unwrap_err();
        assert_eq!(err.get_inner().status_code(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_delete_active_wallet_blocked() {
        let ctx = test_ctx();
        let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let password = "pw123";

        // Create wallet
        block_on(create_wallet_rpc(ctx.clone(), CreateWalletRequest {
            wallet_name: "active-wallet".to_string(),
            password: password.to_string(),
            mnemonic: mnemonic.to_string(),
        }))
        .unwrap();

        // Set it as active
        let _ = ctx.wallet_name.pin(Some("active-wallet".to_string()));

        // Delete should be blocked
        let err = block_on(delete_wallet_rpc(ctx.clone(), DeleteWalletRequest {
            wallet_name: "active-wallet".to_string(),
            password: password.to_string(),
        }))
        .unwrap_err();
        assert_eq!(err.get_inner().status_code(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_initialize_wallet_passphrase_new_wallet() {
        let ctx = test_ctx();
        let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let password = "init_test_pw";

        // First-save of a supplied plaintext seed: returns the resolved seed.
        let result = block_on(initialize_wallet_passphrase(
            &ctx,
            PassphraseForm::Plaintext(mnemonic.to_string()),
            Some("init-wallet"),
            Some(password),
        ));
        assert_eq!(result.unwrap(), Some(mnemonic.to_string()));

        // wallet_name should be set on ctx
        assert_eq!(ctx.wallet_name.as_option(), Some(&Some("init-wallet".to_string())));

        // File should exist
        let list = block_on(get_wallet_names_rpc(ctx.clone(), GetWalletNamesRequest {})).unwrap();
        assert!(list.wallet_names.contains(&"init-wallet".to_string()));
    }

    #[test]
    fn test_initialize_wallet_passphrase_anonymous_mode() {
        let ctx = test_ctx();

        let result = block_on(initialize_wallet_passphrase(&ctx, PassphraseForm::Absent, None, None));
        assert_eq!(result.unwrap(), None);
        assert_eq!(ctx.wallet_name.as_option(), Some(&None));
    }

    #[test]
    fn test_initialize_wallet_passphrase_legacy_plaintext_no_name() {
        let ctx = test_ctx();
        let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

        // Legacy passphrase-only: plaintext seed used directly, slot pinned None.
        let result = block_on(initialize_wallet_passphrase(
            &ctx,
            PassphraseForm::Plaintext(mnemonic.to_string()),
            None,
            None,
        ));
        assert_eq!(result.unwrap(), Some(mnemonic.to_string()));
        assert_eq!(ctx.wallet_name.as_option(), Some(&None));
    }

    /// T5 regression: a re-login that supplies only the stored name and password
    /// (no passphrase) MUST load the stored seed without any equality comparison
    /// and resolve the same seed as the first start.
    #[test]
    fn test_initialize_wallet_passphrase_relogin_loads_stored_seed() {
        let ctx = test_ctx();
        let password = "relogin_pw";

        // First start: generate-and-persist (passphrase absent, no stored file).
        let first = block_on(initialize_wallet_passphrase(
            &ctx,
            PassphraseForm::Absent,
            Some("relogin-wallet"),
            Some(password),
        ))
        .unwrap();
        let seed = first.expect("first start must resolve a generated seed");

        // Second start: passphrase absent, stored file present => load-and-use.
        let second = block_on(initialize_wallet_passphrase(
            &ctx,
            PassphraseForm::Absent,
            Some("relogin-wallet"),
            Some(password),
        ))
        .unwrap();
        assert_eq!(second, Some(seed));
    }

    /// T5: a re-login with an incorrect wallet_password against an existing record
    /// MUST abort with a decryption/mnemonic error (InvalidPassword), distinct
    /// from the passphrase-mismatch case (R29).
    #[test]
    fn test_initialize_wallet_passphrase_relogin_wrong_password() {
        let ctx = test_ctx();
        let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

        block_on(initialize_wallet_passphrase(
            &ctx,
            PassphraseForm::Plaintext(mnemonic.to_string()),
            Some("wrongpw-wallet"),
            Some("correct_pw"),
        ))
        .unwrap();

        let err = block_on(initialize_wallet_passphrase(
            &ctx,
            PassphraseForm::Absent,
            Some("wrongpw-wallet"),
            Some("incorrect_pw"),
        ))
        .unwrap_err();
        assert_eq!(err.get_inner().status_code(), StatusCode::BAD_REQUEST);
        assert!(matches!(err.get_inner(), WalletError::InvalidPassword));
    }

    /// T5: a start that supplies a different plaintext passphrase than the stored
    /// seed MUST abort with a passphrase-mismatch error (genuine seed conflict).
    #[test]
    fn test_initialize_wallet_passphrase_genuine_conflict() {
        let ctx = test_ctx();
        let stored = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let other = "legal winner thank year wave sausage worth useful legal winner thank yellow";
        let password = "conflict_pw";

        block_on(initialize_wallet_passphrase(
            &ctx,
            PassphraseForm::Plaintext(stored.to_string()),
            Some("conflict-wallet"),
            Some(password),
        ))
        .unwrap();

        let err = block_on(initialize_wallet_passphrase(
            &ctx,
            PassphraseForm::Plaintext(other.to_string()),
            Some("conflict-wallet"),
            Some(password),
        ))
        .unwrap_err();
        assert_eq!(err.get_inner().status_code(), StatusCode::BAD_REQUEST);
        assert!(matches!(err.get_inner(), WalletError::InvalidRequest(_)));
    }

    /// T5: a plaintext passphrase that matches the stored seed confirms and proceeds.
    #[test]
    fn test_initialize_wallet_passphrase_plaintext_confirm() {
        let ctx = test_ctx();
        let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let password = "confirm_pw";

        block_on(initialize_wallet_passphrase(
            &ctx,
            PassphraseForm::Plaintext(mnemonic.to_string()),
            Some("confirm-wallet"),
            Some(password),
        ))
        .unwrap();

        let again = block_on(initialize_wallet_passphrase(
            &ctx,
            PassphraseForm::Plaintext(mnemonic.to_string()),
            Some("confirm-wallet"),
            Some(password),
        ))
        .unwrap();
        assert_eq!(again, Some(mnemonic.to_string()));
    }

    /// T5: an encrypted re-login (supplied envelope, stored file present) confirms
    /// and proceeds when the decrypted seeds match.
    #[test]
    fn test_initialize_wallet_passphrase_encrypted_confirm() {
        let ctx = test_ctx();
        let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let password = "encrypted_pw";

        // First-save establishes the stored record.
        block_on(initialize_wallet_passphrase(
            &ctx,
            PassphraseForm::Plaintext(mnemonic.to_string()),
            Some("encrypted-wallet"),
            Some(password),
        ))
        .unwrap();

        // Supply the same seed as an encrypted envelope: confirm-and-proceed.
        let supplied = encrypt_mnemonic(mnemonic, password).unwrap();
        let resolved = block_on(initialize_wallet_passphrase(
            &ctx,
            PassphraseForm::Encrypted(supplied),
            Some("encrypted-wallet"),
            Some(password),
        ))
        .unwrap();
        assert_eq!(resolved, Some(mnemonic.to_string()));
    }

    /// T5: an encrypted passphrase without a wallet_name refuses to start.
    #[test]
    fn test_initialize_wallet_passphrase_encrypted_without_name_refused() {
        let ctx = test_ctx();
        let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let supplied = encrypt_mnemonic(mnemonic, "any_pw").unwrap();

        let err = block_on(initialize_wallet_passphrase(
            &ctx,
            PassphraseForm::Encrypted(supplied),
            None,
            None,
        ))
        .unwrap_err();
        assert_eq!(err.get_inner().status_code(), StatusCode::BAD_REQUEST);
        assert!(matches!(err.get_inner(), WalletError::InvalidRequest(_)));
    }

    /// T5: a wallet_name without wallet_password refuses to start.
    #[test]
    fn test_initialize_wallet_passphrase_missing_password_refused() {
        let ctx = test_ctx();

        let err = block_on(initialize_wallet_passphrase(
            &ctx,
            PassphraseForm::Absent,
            Some("no-password-wallet"),
            None,
        ))
        .unwrap_err();
        assert_eq!(err.get_inner().status_code(), StatusCode::BAD_REQUEST);
        assert!(matches!(err.get_inner(), WalletError::InvalidRequest(_)));
    }

    const TEST_MNEMONIC: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    /// Create writes the canonical six-field `.json` envelope directly in the
    /// database root — not under a `wallets/` subdirectory and not as `.wallet`.
    #[test]
    fn test_create_writes_json_in_db_root() {
        let ctx = test_ctx();
        block_on(create_wallet_rpc(ctx.clone(), CreateWalletRequest {
            wallet_name: "root-wallet".to_string(),
            password: "pw123".to_string(),
            mnemonic: TEST_MNEMONIC.to_string(),
        }))
        .unwrap();

        let json_path = ctx.db_root().join("root-wallet.json");
        assert!(json_path.exists(), "create must write <db_root>/<name>.json");
        assert!(
            !ctx.wallets_dir().join("root-wallet.wallet").exists(),
            "create must NOT write a legacy .wallet file"
        );

        // The on-disk record is the canonical six-field envelope.
        let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&json_path).unwrap()).unwrap();
        let obj = value.as_object().unwrap();
        assert_eq!(obj["version"], json!(1));
        assert_eq!(obj["encryption_algorithm"], json!("AES256CBC"));
        assert!(obj.contains_key("key_derivation_details"));
        assert!(obj["iv"].is_string());
        assert!(obj["ciphertext"].is_string());
        assert!(obj["tag"].is_string());
    }

    /// Legacy-read: a record written by the previous reloaded build
    /// (`<db_root>/wallets/<name>.wallet`, two-field envelope) MUST still be
    /// listed and decryptable, while new creates keep emitting `.json`.
    #[test]
    fn test_legacy_wallet_read_compat() {
        let ctx = test_ctx();
        let password = "legacy_pw";

        // Reconstruct a legacy `.wallet` record using the retained legacy path.
        let legacy = crypto::legacy_encrypt_mnemonic(TEST_MNEMONIC, password).unwrap();
        let legacy_dir = ctx.wallets_dir();
        std::fs::create_dir_all(&legacy_dir).unwrap();
        let legacy_path = legacy_dir.join("legacy-wallet.wallet");
        std::fs::write(&legacy_path, serde_json::to_string(&legacy).unwrap()).unwrap();

        // The legacy wallet is discoverable.
        let list = block_on(get_wallet_names_rpc(ctx.clone(), GetWalletNamesRequest {})).unwrap();
        assert!(list.wallet_names.contains(&"legacy-wallet".to_string()));

        // A new create still emits `.json` in the root (never `.wallet`).
        block_on(create_wallet_rpc(ctx.clone(), CreateWalletRequest {
            wallet_name: "fresh-wallet".to_string(),
            password: "fresh_pw_123".to_string(),
            mnemonic: TEST_MNEMONIC.to_string(),
        }))
        .unwrap();
        assert!(ctx.db_root().join("fresh-wallet.json").exists());

        // Wrong password against the legacy record fails and leaves it intact.
        let err = block_on(delete_wallet_rpc(ctx.clone(), DeleteWalletRequest {
            wallet_name: "legacy-wallet".to_string(),
            password: "wrong".to_string(),
        }))
        .unwrap_err();
        assert!(matches!(err.get_inner(), WalletError::InvalidPassword));
        assert!(legacy_path.exists());

        // Correct password decrypts and deletes the legacy record.
        block_on(delete_wallet_rpc(ctx.clone(), DeleteWalletRequest {
            wallet_name: "legacy-wallet".to_string(),
            password: password.to_string(),
        }))
        .unwrap();
        assert!(!legacy_path.exists());
    }

    /// Listing scans the database root non-recursively (ignoring per-identity
    /// subdirectories) and trims whitespace from file stems (§7.5 R8A).
    #[test]
    fn test_listing_non_recursive_and_trims_stems() {
        let ctx = test_ctx();
        let root = ctx.db_root();
        std::fs::create_dir_all(&root).unwrap();

        // A canonical record whose on-disk stem carries surrounding whitespace.
        let env = crypto::encrypt_mnemonic(TEST_MNEMONIC, "pw").unwrap();
        std::fs::write(root.join("  spaced  .json"), serde_json::to_string(&env).unwrap()).unwrap();

        // A per-identity-style subdirectory with a `.json` that MUST NOT be listed.
        let subdir = root.join("00deadbeef");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::write(subdir.join("hidden.json"), b"{}").unwrap();

        let list = block_on(get_wallet_names_rpc(ctx.clone(), GetWalletNamesRequest {})).unwrap();
        assert!(
            list.wallet_names.contains(&"spaced".to_string()),
            "stem must be trimmed"
        );
        assert!(
            !list.wallet_names.contains(&"hidden".to_string()),
            "must not descend into per-identity subdirectories"
        );
    }

    /// T4: tampering a byte of the on-disk record's ciphertext MUST surface as
    /// `InvalidPassword` on `delete_wallet`, never a panic or UTF-8 error, and
    /// MUST leave the record in place.
    #[test]
    fn test_on_disk_tamper_rejected_on_delete() {
        let ctx = test_ctx();
        let password = "tamper_pw";
        block_on(create_wallet_rpc(ctx.clone(), CreateWalletRequest {
            wallet_name: "tamper".to_string(),
            password: password.to_string(),
            mnemonic: TEST_MNEMONIC.to_string(),
        }))
        .unwrap();

        let path = ctx.db_root().join("tamper.json");
        let mut value: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        // Flip the first Base64 character of the ciphertext (stays valid Base64).
        let ct = value["ciphertext"].as_str().unwrap().to_string();
        let mut chars: Vec<char> = ct.chars().collect();
        chars[0] = if chars[0] == 'A' { 'B' } else { 'A' };
        value["ciphertext"] = json!(chars.into_iter().collect::<String>());
        std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();

        let err = block_on(delete_wallet_rpc(ctx.clone(), DeleteWalletRequest {
            wallet_name: "tamper".to_string(),
            password: password.to_string(),
        }))
        .unwrap_err();
        assert!(matches!(err.get_inner(), WalletError::InvalidPassword));
        assert!(path.exists(), "a failed password check must leave the record intact");
    }
}
