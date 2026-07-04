use super::{SwapEvent, SwapsContext};
use chain::hash::H256;
use common::now_ms;
use http::Response;
use mm2_core::mm_ctx::MmArc;
use rpc::v1::types::H256 as H256Json;
use serde_json::{self as json, Value as Json};
use std::collections::hash_map::{Entry, HashMap};
use uuid::Uuid;

/// One-hour penalty (in seconds) applied to automatic bans triggered by a failed swap.
const FAILED_SWAP_BAN_SECS: u64 = 3600;

#[derive(Serialize)]
#[serde(tag = "type")]
#[allow(clippy::large_enum_variant)]
pub enum BanReason {
    Manual {
        reason: String,
    },
    FailedSwap {
        caused_by_swap: Uuid,
        caused_by_event: SwapEvent,
    },
}

/// A pubkey ban entry stored in the ban map. The `reason` carries the wire-visible
/// ban description, while `expires_at` (epoch seconds) optionally bounds its lifetime.
/// `None` means the ban is permanent until an explicit unban.
pub struct BannedPubkey {
    reason: BanReason,
    expires_at: Option<u64>,
}

/// Removes any expired entries from the ban map, comparing each entry's `expires_at`
/// (epoch seconds) against the current time. Permanent entries (`None`) are retained.
fn purge_expired(banned: &mut HashMap<H256Json, BannedPubkey>) {
    let now = now_ms() / 1000;
    banned.retain(|_, entry| match entry.expires_at {
        Some(expires_at) => expires_at > now,
        None => true,
    });
}

/// Builds a serializable map of pubkey hash to the bare `BanReason`, preserving the
/// existing wire shape (the stored expiry wrapper is not exposed over the wire).
fn bare_reasons(banned: &HashMap<H256Json, BannedPubkey>) -> HashMap<&H256Json, &BanReason> {
    banned.iter().map(|(pubkey, entry)| (pubkey, &entry.reason)).collect()
}

pub fn ban_pubkey_on_failed_swap(ctx: &MmArc, pubkey: H256, swap_uuid: &Uuid, event: SwapEvent) {
    let ctx = SwapsContext::from_ctx(ctx).unwrap();
    let mut banned = ctx.banned_pubkeys.lock().unwrap();
    banned.insert(pubkey.into(), BannedPubkey {
        reason: BanReason::FailedSwap {
            caused_by_swap: *swap_uuid,
            caused_by_event: event,
        },
        expires_at: Some(now_ms() / 1000 + FAILED_SWAP_BAN_SECS),
    });
}

pub fn is_pubkey_banned(ctx: &MmArc, pubkey: &H256Json) -> bool {
    let ctx = SwapsContext::from_ctx(ctx).unwrap();
    let mut banned = ctx.banned_pubkeys.lock().unwrap();
    purge_expired(&mut banned);
    banned.contains_key(pubkey)
}

pub async fn list_banned_pubkeys_rpc(ctx: MmArc) -> Result<Response<Vec<u8>>, String> {
    let ctx = try_s!(SwapsContext::from_ctx(&ctx));
    let mut banned_pubs = try_s!(ctx.banned_pubkeys.lock());
    purge_expired(&mut banned_pubs);
    let res = try_s!(json::to_vec(&json!({
        "result": bare_reasons(&banned_pubs),
    })));
    Ok(try_s!(Response::builder().body(res)))
}

#[derive(Deserialize)]
struct BanPubkeysReq {
    pubkey: H256Json,
    reason: String,
    /// Optional ban lifetime in minutes. When present the ban auto-clears after this
    /// many minutes; when absent the ban is permanent until an explicit unban.
    #[serde(default)]
    duration_min: Option<u64>,
}

pub async fn ban_pubkey_rpc(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let req: BanPubkeysReq = try_s!(json::from_value(req));
    let ctx = try_s!(SwapsContext::from_ctx(&ctx));
    let mut banned_pubs = try_s!(ctx.banned_pubkeys.lock());
    purge_expired(&mut banned_pubs);

    match banned_pubs.entry(req.pubkey) {
        Entry::Occupied(_) => ERR!("Pubkey is banned already"),
        Entry::Vacant(entry) => {
            let expires_at = req.duration_min.map(|minutes| now_ms() / 1000 + minutes * 60);
            entry.insert(BannedPubkey {
                reason: BanReason::Manual { reason: req.reason },
                expires_at,
            });
            let res = try_s!(json::to_vec(&json!({
                "result": "success",
            })));
            Ok(try_s!(Response::builder().body(res)))
        },
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", content = "data")]
enum UnbanPubkeysReq {
    All,
    Few(Vec<H256Json>),
}

pub async fn unban_pubkeys_rpc(ctx: MmArc, req: Json) -> Result<Response<Vec<u8>>, String> {
    let req: UnbanPubkeysReq = try_s!(json::from_value(req["unban_by"].clone()));
    let ctx = try_s!(SwapsContext::from_ctx(&ctx));
    let mut banned_pubs = try_s!(ctx.banned_pubkeys.lock());
    purge_expired(&mut banned_pubs);
    let mut unbanned = HashMap::new();
    let mut were_not_banned = vec![];
    match req {
        UnbanPubkeysReq::All => {
            unbanned = banned_pubs.drain().collect();
        },
        UnbanPubkeysReq::Few(pubkeys) => {
            for pubkey in pubkeys {
                match banned_pubs.remove(&pubkey) {
                    Some(removed) => {
                        unbanned.insert(pubkey, removed);
                    },
                    None => were_not_banned.push(pubkey),
                }
            }
        },
    }
    let res = try_s!(json::to_vec(&json!({
        "result": {
            "still_banned": bare_reasons(&banned_pubs),
            "unbanned": bare_reasons(&unbanned),
            "were_not_banned": were_not_banned,
        },
    })));
    Ok(try_s!(Response::builder().body(res)))
}

#[cfg(test)]
mod tests {
    use super::super::maker_swap::MakerSwapEvent;
    use super::*;
    use common::block_on;
    use mm2_core::mm_ctx::MmCtxBuilder;

    /// Builds a distinct pubkey hash from a single repeated byte, via the same hex
    /// representation the RPC layer accepts.
    fn pubkey(byte: u8) -> H256Json {
        let hex: String = std::iter::repeat(format!("{:02x}", byte)).take(32).collect();
        json::from_value(json!(hex)).unwrap()
    }

    /// Reads the parsed `result` map from a `list_banned_pubkeys` / `unban_pubkeys` response body.
    fn result_of(response: Response<Vec<u8>>) -> Json {
        let body: Json = json::from_slice(response.body()).unwrap();
        body["result"].clone()
    }

    /// Returns the single value of a one-entry JSON object, avoiding any dependency on the
    /// exact wire encoding of the pubkey-hash map key.
    fn only_value(map: &Json) -> Json {
        let obj = map.as_object().unwrap();
        assert_eq!(obj.len(), 1);
        obj.values().next().unwrap().clone()
    }

    /// Forces the stored expiry of a banned pubkey to a moment in the past, simulating elapsed time.
    fn expire_now(ctx: &MmArc, pubkey: &H256Json) {
        let swaps_ctx = SwapsContext::from_ctx(ctx).unwrap();
        let mut banned = swaps_ctx.banned_pubkeys.lock().unwrap();
        banned.get_mut(pubkey).unwrap().expires_at = Some(now_ms() / 1000 - 1);
    }

    #[test]
    fn duration_ban_disappears_after_expiry() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let pk = pubkey(1);
        let req = json!({ "pubkey": pk, "reason": "spammer", "duration_min": 10 });
        block_on(ban_pubkey_rpc(ctx.clone(), req)).unwrap();
        assert!(is_pubkey_banned(&ctx, &pk));

        // Simulate the TTL elapsing.
        expire_now(&ctx, &pk);

        // Enforcement and listing both drop the expired entry without an explicit unban.
        assert!(!is_pubkey_banned(&ctx, &pk));
        let listed = result_of(block_on(list_banned_pubkeys_rpc(ctx)).unwrap());
        assert_eq!(listed, json!({}));
    }

    #[test]
    fn permanent_ban_persists_without_duration() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let pk = pubkey(2);
        let req = json!({ "pubkey": pk, "reason": "bad actor" });
        block_on(ban_pubkey_rpc(ctx.clone(), req)).unwrap();

        // No duration means a permanent (None) expiry that survives purging.
        let swaps_ctx = SwapsContext::from_ctx(&ctx).unwrap();
        assert_eq!(
            swaps_ctx.banned_pubkeys.lock().unwrap().get(&pk).unwrap().expires_at,
            None
        );
        drop(swaps_ctx);

        assert!(is_pubkey_banned(&ctx, &pk));
        let listed = result_of(block_on(list_banned_pubkeys_rpc(ctx)).unwrap());
        assert_eq!(only_value(&listed)["type"], json!("Manual"));
    }

    #[test]
    fn failed_swap_ban_carries_one_hour_expiry() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let pk_chain = H256::default();
        let uuid = Uuid::new_v4();
        let before = now_ms() / 1000;
        ban_pubkey_on_failed_swap(&ctx, pk_chain, &uuid, SwapEvent::Maker(MakerSwapEvent::Finished));
        let after = now_ms() / 1000;

        let swaps_ctx = SwapsContext::from_ctx(&ctx).unwrap();
        let banned = swaps_ctx.banned_pubkeys.lock().unwrap();
        let entry = banned.values().next().expect("one failed-swap ban present");
        let expires_at = entry.expires_at.expect("failed-swap ban must be time-limited");
        assert!(expires_at >= before + FAILED_SWAP_BAN_SECS);
        assert!(expires_at <= after + FAILED_SWAP_BAN_SECS);
    }

    #[test]
    fn reban_rejected_while_live_but_allowed_after_expiry() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let pk = pubkey(4);
        let req = || json!({ "pubkey": pk, "reason": "first", "duration_min": 5 });
        block_on(ban_pubkey_rpc(ctx.clone(), req())).unwrap();

        // A live ban blocks re-banning.
        assert!(block_on(ban_pubkey_rpc(ctx.clone(), req())).is_err());

        // Once expired, the same pubkey can be banned again.
        expire_now(&ctx, &pk);
        block_on(ban_pubkey_rpc(ctx.clone(), json!({ "pubkey": pk, "reason": "second" }))).unwrap();
        assert!(is_pubkey_banned(&ctx, &pk));
    }

    #[test]
    fn list_and_unban_emit_bare_ban_reason() {
        let ctx = MmCtxBuilder::default().into_mm_arc();
        let pk = pubkey(5);
        block_on(ban_pubkey_rpc(
            ctx.clone(),
            json!({ "pubkey": pk, "reason": "wire shape" }),
        ))
        .unwrap();

        // The list value is the bare type-tagged BanReason, not a wrapper object.
        let listed = result_of(block_on(list_banned_pubkeys_rpc(ctx.clone())).unwrap());
        assert_eq!(only_value(&listed), json!({ "type": "Manual", "reason": "wire shape" }));

        // Unban responses keep the same bare BanReason shape under `unbanned`.
        let unban_req = json!({ "unban_by": { "type": "All" } });
        let unban_res = result_of(block_on(unban_pubkeys_rpc(ctx, unban_req)).unwrap());
        assert_eq!(
            only_value(&unban_res["unbanned"]),
            json!({ "type": "Manual", "reason": "wire shape" })
        );
        assert_eq!(unban_res["still_banned"], json!({}));
    }
}
