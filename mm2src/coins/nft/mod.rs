//! Non-fungible token (NFT) support for KDF-RELOADED.
//!
//! This module groups everything that is required to keep track of NFT
//! ownership, transfer history and withdrawals on the EVM-family chains
//! that are supported by the framework. The intent is to expose a small,
//! well-typed surface that the dispatcher and storage backends can build
//! on top of, while keeping the network and persistence layers swappable.
//!
//! Sub-modules:
//! * [`model`] — public request/response/data types and the small
//!   enums (chain, contract type, transfer status, …) that they share.
//! * [`errors`] — error hierarchies returned by the upcoming RPC handlers.
//! * [`serde_helpers`] — ser/de helpers for fields that travel as JSON
//!   strings (token IDs, optional `BigUint` amounts).
//! * [`store`] — storage trait layer plus a SQLite backend (native).
//! * [`providers`] — URL/spam helpers and a thin HTTP client wrapper
//!   used to talk to external NFT metadata services.
//!
//! The IndexedDB backend and RPC handlers are added in subsequent
//! P10.3.x phases.

pub mod activation;
pub mod context;
pub mod errors;
pub mod model;
pub mod providers;
#[cfg(not(target_arch = "wasm32"))] pub mod rpc;
pub mod serde_helpers;
pub mod store;
#[cfg(not(target_arch = "wasm32"))] pub mod withdraw;

pub use context::NftCtx;

pub use activation::{enable_nft, EnableNftRequest, EnableNftResponse, NftActivationParams, NftActivationProtocol,
                     NftProtocolData, NftProvider, NftProviderInfo};
pub use errors::{ClearNftDbError, EnableNftError, GetNftInfoError, LockDbError, MetadataFetchError, ParseChainError,
                 ParseContractTypeError, ParseTransferStatusError, SpamFilterError, TransferConfirmationsError,
                 UpdateNftError, UpdateSpamPhishingError};
pub use model::{Chain, ChainTicker, ClearNftDbReq, ContractType, Nft, NftCommon, NftInfo, NftList, NftListFilters,
                NftListReq, NftMetadataReq, NftTokenIdent, NftTransfer, NftTransferCommon, NftTransferList,
                NftTransfersFilters, NftTransfersReq, RefreshMetadataReq, TransferMeta, TransferStatus, UpdateNftReq,
                UriMeta, WithdrawErc1155, WithdrawErc721, WithdrawNftReq};
pub use providers::{apply_spam_protection_to_nft, apply_spam_protection_to_transfer, decamouflage_legacy_ipfs_url,
                    domain_of, fetch_json as fetch_provider_json, normalise_metadata_urls,
                    FetchError as ProviderFetchError, SpamScanError};
pub use store::{NftHistoryStore, NftListStore, NftStoreError, RemoveOutcome};
