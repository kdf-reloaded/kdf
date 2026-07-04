//! Lazy-initialised NFT subsystem context attached to [`MmArc`].
//!
//! The context wraps the storage backend so RPC handlers don't have to
//! plumb the underlying SQLite/IndexedDB connection through every call
//! site. The wrapper is registered into [`MmCtx::nft_ctx`] on first
//! access and reused thereafter.
//!
//! Only the native (SQLite) backend is wired in this revision; the WASM
//! backend will be added together with P10.3.4 once the workspace
//! `wasm32-unknown-unknown` build is repaired.

use crate::nft::model::Chain;
#[cfg(target_arch = "wasm32")]
use crate::nft::store::idb::{IndexedDbNftStore, NftIndexedDb};
#[cfg(not(target_arch = "wasm32"))]
use crate::nft::store::sqlite::SqliteNftStore;
use mm2_core::mm_ctx::{from_ctx, MmArc};
#[cfg(target_arch = "wasm32")]
use mm2_db::indexed_db::ConstructibleDb;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// Central NFT context held by the application. One instance per
/// [`MmArc`] is created lazily on first access.
pub struct NftCtx {
    /// SQLite-backed storage handle. Cloned by [`Self::store`].
    #[cfg(not(target_arch = "wasm32"))]
    store: SqliteNftStore,
    /// IndexedDB-backed storage handle (WASM target).
    #[cfg(target_arch = "wasm32")]
    store: IndexedDbNftStore,
    /// Chains for which `enable_nft` has already brought the subsystem
    /// into existence. Used to reject a second activation of the same
    /// ticker.
    activated: Mutex<HashSet<Chain>>,
}

impl NftCtx {
    /// Look up (or lazily create) the [`NftCtx`] attached to `ctx`.
    ///
    /// On native targets the underlying async SQLite connection must
    /// already be initialised (`MmCtx::async_sqlite_connection`); this is
    /// done by the standard MM init path before any RPC handler runs.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_mm_ctx(ctx: &MmArc) -> Result<Arc<NftCtx>, String> {
        from_ctx(&ctx.nft_ctx, move || {
            let conn_handle = ctx
                .async_sqlite_connection
                .get()
                .ok_or_else(|| "async_sqlite_connection is not initialized".to_owned())?
                .clone();
            // `AsyncConnection` is internally a clonable handle to a
            // background worker thread; the surrounding `AsyncMutex` is
            // historical and not required here. We block briefly to clone
            // the underlying handle so the store can issue calls without
            // contending on the unrelated mutex used by other subsystems.
            let conn = futures::executor::block_on(conn_handle.lock()).clone();
            Ok(NftCtx {
                store: SqliteNftStore::new(Arc::new(conn)),
                activated: Mutex::new(HashSet::new()),
            })
        })
    }

    /// Look up (or lazily create) the [`NftCtx`] attached to `ctx` on
    /// `wasm32`. The IndexedDB instance is itself constructed lazily;
    /// the [`SharedDb`] wrapper held here only holds a `None` slot until
    /// the first storage call triggers `get_or_initialize`.
    #[cfg(target_arch = "wasm32")]
    pub fn from_mm_ctx(ctx: &MmArc) -> Result<Arc<NftCtx>, String> {
        from_ctx(&ctx.nft_ctx, move || {
            let shared = ConstructibleDb::<NftIndexedDb>::new_shared(ctx);
            Ok(NftCtx {
                store: IndexedDbNftStore::new(shared),
                activated: Mutex::new(HashSet::new()),
            })
        })
    }

    /// Borrow the SQLite-backed store. The store itself is `Clone` so
    /// callers can move a handle into spawned futures when needed.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn store(&self) -> &SqliteNftStore { &self.store }

    /// Borrow the IndexedDB-backed store on `wasm32`.
    #[cfg(target_arch = "wasm32")]
    pub fn store(&self) -> &IndexedDbNftStore { &self.store }

    /// Returns `true` when `enable_nft` has already activated the NFT
    /// subsystem for `chain`.
    pub fn is_activated(&self, chain: Chain) -> bool {
        self.activated
            .lock()
            .expect("nft activation set poisoned")
            .contains(&chain)
    }

    /// Record `chain` as activated. Returns `true` when this call is the
    /// one that flipped the chain from inactive to active.
    pub fn mark_activated(&self, chain: Chain) -> bool {
        self.activated
            .lock()
            .expect("nft activation set poisoned")
            .insert(chain)
    }
}
