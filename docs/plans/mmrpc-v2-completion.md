# Plan: finish mmrpc 2.0 support for the legacy-only RPC methods

**Status:** planning (candidate for the next version; not scheduled) — **Last reviewed:** 2026-11-09

> **Context:** written 2026-11-09, after CRD ch.43 §43.1A (PR #312) bound which methods exist on
> which request surface. This plan proposes a Reloaded extension: versioned (`mmrpc: "2.0"`) forms of
> methods that are legacy-only in KDF Reloaded and in the reference. It reverses no decision of
> v0.4.0. Nothing here is executed yet.

## Why

Wallet SDKs increasingly build every request in the versioned envelope. The Stashi wallet, for
example, sends `setprice`, `cancel_order` and `my_orders` as versioned requests. These methods exist
only on the legacy surface, both in the reference and in KDF Reloaded, so each such call answers
`NoSuchMethod` (HTTP 400) on every KDF node today (CRD ch.43 R43.1A.3, R43.1A.4, R43.1A.9). A wallet
author then has to keep two request builders, one per envelope, and learn per method which one
applies.

For v0.4.0 the maintainer decided on 2026-11-08 to keep the reference's surfaces: the legacy-only
order methods stay legacy-only, and no versioned aliases are added (CRD ch.43 D43.1A.1, option (i)).
PR #312 also implemented the reference's one-way fallback: a legacy request whose method the
legacy table does not know is retried on the versioned table (R43.1A.1a–R43.1A.1e, PR #312). That
lets a legacy-only client reach versioned-only methods. Nothing yet helps a versioned-only client
reach the legacy-only methods. This plan closes that gap for the next version, as a documented
extension.

## What "finish mmrpc 2.0 support" means here

- Every method that is reachable only through a legacy request today gets a versioned form under the
  same wire name, unless §1.4 keeps it legacy-only.
- The legacy form stays exactly as it is. No existing request changes its answer, except that a
  versioned request for one of these names stops answering `NoSuchMethod`.
- A versioned form follows the versioned envelope's rules in full:
  - strict top-level keys (R43.1A.2);
  - parameters only inside `params`;
  - `id` echoed (chapter 55 R55.1.1–R55.1.2);
  - typed errors with a bound HTTP status (R55.1.5);
  - numbers in the forms the versioned surface already uses.
- It is a Reloaded extension. The reference has no versioned form of these methods at any reference
  state checked (R43.1A.9), so there is no reference shape to match. Each versioned form is recorded
  as a deliberate divergence (`rpc-api-reference-wire-parity.md` §2.2).

## Sources and how they are used (clean-room)

1. **CRD ch.43 §43.1A** binds the surfaces and the envelope: the surface table of R43.1A.4, the
   envelope strictness, the dispatcher errors, and the versioned shapes of `orderbook`, `best_orders`
   and `my_recent_swaps`. It passed the KDF Dirty Gate on 2026-11-08 and is on dev. No further
   reference input is needed for the extension itself.
2. **`docs/reloaded-rewrite/rpc-method-census.md`** lists every method name per surface; it is the
   inventory source, reconciled with Reloaded's dispatchers. On dev as of 2026-11-09 the legacy table
   routes 54 names, 10 of them on both surfaces, which leaves 44 legacy-only names. The census counts
   52 and 9. It does not list `peer_connection_healthcheck` on the legacy surface, where Reloaded
   also routes it, nor the test helper `sim_panic`. *(Amended 2026-11-10: 12 of the 54 names are on both
   surfaces, since `max_taker_vol` and `min_trading_vol` are also routed on V2 here, so 42 are
   legacy-only. `sim_panic` is now compiled only with the test-only Cargo feature `sim-panic-rpc`, so a
   shipped build's legacy table routes 53 names, 41 of them legacy-only; see CRD ch.43 §43.4 status
   note.)*
3. **`docs/plans/rpc-api-reference-wire-parity.md`** supplies:
   - the seven wire dimensions W1–W7 that each versioned form binds;
   - the divergence record of §2.2;
   - the batch grouping of §1.5, which this plan's phases follow.
4. **Reloaded's own dispatchers** (`rpc/dispatcher/dispatcher.rs`, `dispatcher_legacy.rs`) and each
   method's legacy handler give the request members and the success shape that a versioned form
   wraps.
5. **Client request shapes.** The shapes a wallet such as the Stashi wallet actually sends are read
   from its public source, after a license check, for request shapes only. They decide D2 below and
   are recorded in the governing chapter.

Steps 1–4 use no forbidden-class input. Step 5 reads a third-party client, not the KDF corpus.

## Part 1 — The versioned forms

### 1.1 Inventory

The legacy-only names, grouped as in the parity plan's batches:

| Group | Methods |
| --- | --- |
| A. Order management | `setprice`, `update_maker_order`, `cancel_order`, `cancel_all_orders`, `my_orders`, `order_status` |
| B. Trading | `buy`, `sell`, `orderbook_depth`, `max_taker_vol`, `min_trading_vol`, `get_trade_fee`, `orders_history_by_filter` |
| C. Swaps | `all_swaps_uuids_by_filter`, `import_swaps`, `recover_funds_of_swap`, `stats_swap_status` |
| D. Wallet and coins | `my_balance`, `disable_coin`, `send_raw_transaction`, `validateaddress`, `convertaddress`, `convert_utxo_address`, `kmd_rewards_info`, `coins_needed_for_kick_start`, `set_required_confirmations`, `set_requires_notarization` |
| E. Daemon and network | `version`, `stop`, `metrics`, `get_my_peer_id`, `get_directly_connected_peers`, `get_gossip_mesh`, `get_gossip_peer_topics`, `get_gossip_topic_peers`, `get_relay_mesh`, `ban_pubkey`, `list_banned_pubkeys`, `unban_pubkeys` |
| Legacy-only for good (§1.4) | `electrum`, `enable`, `help`, `show_priv_key`, `sim_panic` |

The phase that takes a group first reconciles its list with Reloaded's dispatchers of that day.

### 1.2 Shape of a versioned form

1. **Request.** The members the legacy form takes inline move into `params`, with the same names,
   types and defaults (D2 decides the alternative). A versioned request whose `params` carries an
   unknown member answers `InvalidRequest`, as every versioned method does today.
2. **Success.**
   - The legacy success content goes under `result`. For a legacy body of the form
     `{"result": X}`, `result` is `X`. For a legacy body that is a bare object, `result` is that
     object.
   - Numbers keep the encoding the legacy form uses, unless the group's chapter binds the versioned
     multi-representation numbers (R43.1A.8).
   - Request and response shapes are bound per method in the governing chapter (W1, W2, W7).
3. **Errors.**
   - Each versioned form has a typed error enum. Its `error_type` tokens, `error_data` kinds and
     HTTP statuses are bound per method (W3–W6, the `rpc-api-reference-wire-parity` §2.1 table
     form).
   - Where a versioned sibling already names the same failure, the same token is reused. For
     example, the volume refusals that `trade_preimage` already types.
   - A failure without a class of its own answers `Internal` (string, HTTP 500).
4. **Envelope.** The dispatcher, envelope, local-only and ban checks and authentication are those of
   every versioned method (§43.1A, R43.1A.3). Whether a method is public is unchanged.
5. **Unchanged:**
   - the legacy form and its answers;
   - the legacy-to-versioned fallback (R43.1A.1a–R43.1A.1e), which a known legacy name never
     triggers;
   - the rejected inverse fallback (D43.1A.1 option (iii)).

### 1.3 Switch or none (D1)

The extension only adds answers where the node answers `NoSuchMethod` today, and only for this
node's own API. It touches no wire format between nodes. Under `docs/COMPAT_SWITCHES.md`, "The
default-side rule", it is the second arm, Reloaded's behavior by default.

- **Recommended: no switch.** A client written for the reference never sends these requests, so
  nothing it relies on changes.
- **Alternative: a switch.** One `MM2.json` boolean (for example `mmrpc_v2_legacy_methods`, default
  `true`) for an operator who wants a node that answers exactly as the reference. It costs one more
  setting and a test per phase.

### 1.4 What stays legacy-only, and why

- **`electrum`, `enable`.** Coin activation already has its versioned methods (`enable_*` and
  `task::enable_*`) with a bound contract (CRD ch.55). A versioned alias would be a second activation
  contract with a different request shape.
- **`help`.** It lists the legacy method names. It describes the legacy surface and has no meaning on
  the versioned one.
- **`show_priv_key`.** Key export on the versioned surface goes through `get_private_keys` and its
  refusals (CRD ch.07). A second versioned export path would widen the surface that exposes keys.
- **`sim_panic`.** A test helper that makes the process panic; it is not part of any client's
  surface, and no versioned form is wanted. *(Amended 2026-11-10: it is compiled only with the
  test-only Cargo feature `sim-panic-rpc` and is absent from shipped builds.)*

Each of these is recorded in the governing chapter as "legacy-only by decision", so a later phase
does not reopen it by accident.

### 1.5 Phases and order

One phase per group, one PR per phase, ordered by client impact:

| Phase | Group | Governing chapters |
| --- | --- | --- |
| 1 | A. Order management | 43, 11, 12, 59 |
| 2 | B. Trading | 43, 08, 11, 57, 60 |
| 3 | D. Wallet and coins | 43, 38, 49 and the chapters of the coin families each method serves |
| 4 | C. Swaps | 43, 13, 51, 52 |
| 5 | E. Daemon and network | 43, 28 |

Phase 1 covers the methods the Stashi wallet calls. Each phase merges before the next starts. A
change to a shared piece (the alias registration, the success wrapper, the error base) lands in phase
1, and later phases inherit it.

### 1.6 What each phase ships with

1. **Code.**
   - The versioned registration of the group's names, and one typed error enum per method.
   - The legacy path stays untouched.
2. **Tests (unit).** In the dispatcher's server tests (the `rpc.rs` `server_tests` seam of PR #312):
   - each versioned form answers a success in the 2.0 envelope with `id` echoed;
   - each bound `error_type` has its token, its `error_data` kind and its HTTP status asserted
     (parity plan R43.8.6);
   - the legacy form of each method still answers its legacy shape;
   - an unknown `params` member answers `InvalidRequest`.
3. **Lab row (one per phase).** On a release build, one lab node calls each method of the group in
   both envelopes and compares the answers member by member. A comparison node runs the previous
   dev build, where the versioned call answers `NoSuchMethod`. For phase 1, the row also places, lists
   and cancels an order through the versioned forms only.
4. **Docs.**
   - The governing chapter: a `Deliberate divergence (binding)` note per method, with its W1–W7
     tables.
   - Ch.43: the R43.1A.4 surface table gains a Reloaded column. D43.1A.1 gets a pointer to this plan
     for the next version.
   - `RELOADED_VS_GLEEC.md`: one catalog entry per phase.
   - `docs/GLEEC_COMPATIBILITY.md`: one row per phase, with "none — no switch exists" or the switch.
     `docs/COMPAT_SWITCHES.md` changes only if D1 chooses a switch.
   - `docs/API_REFERENCE_DEVIATIONS.md`: one table per group.
   - CHANGELOG: an Added entry per phase.

### 1.7 Effort and risks

- **Effort.** Phase 1 is the largest, because it builds the shared alias machinery. Later phases are
  mostly per-method shapes and errors.
- **Risk: shape drift.** A versioned form whose shape later differs from a future reference form of
  the same method. Mitigation: if the reference ever adds a versioned form of one of these names,
  that method moves to parity and its alias shape follows the reference (R43.8.2 of the parity plan).
- **Risk: error-mapping depth.** A legacy handler that returns only strings needs its failures
  classified before the typed enum can bind them. D3 bounds how far each phase goes.
- **Risk: clients that probe with `NoSuchMethod`.** A client that uses `NoSuchMethod` to detect a
  legacy-only method and then retries with a legacy request keeps working. Its first call now
  succeeds instead.

## Part 2 — Decisions before phase 1

| # | Question | Options |
| --- | --- | --- |
| D1 | A switch for the extension? | none (recommended, §1.3); one boolean, default on |
| D2 | `params` shape | the legacy members unchanged inside `params` (recommended: one documented shape, easy for SDKs); a redesigned shape per method, for example the versioned number forms |
| D3 | Error depth per phase | the failure classes the handler already distinguishes, the rest `Internal` (recommended); a full classification before each phase ships |
| D4 | Phase order | as §1.5 (recommended); Stashi-first methods only, then stop |

## Order of execution

1. The maintainer decides D1–D4 and schedules the plan for a version.
2. The client request shapes are read (§ "Sources", step 5) and recorded for D2.
3. Phase 1 lands, including the shared machinery and the ch.43 surface-table column.
4. Phases 2–5 land in the order of §1.5, one PR each.

## Out of scope

- Changing any legacy answer.
- Versioned forms of the activation methods `electrum` and `enable` (§1.4).
- The inverse fallback, in which a versioned request for a legacy-only name is retried on the legacy
  table. It stays rejected (D43.1A.1 option (iii)).
- Batch (array) request bodies, which §43.1A records as a gap of its own.
- The `task::`, `stream::`, `gui_storage::`, `lightning::` and `experimental::` routers, which are
  versioned already.
