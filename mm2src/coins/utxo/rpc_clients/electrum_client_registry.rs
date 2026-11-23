//! The registry of the Electrum clients that QTUM and its QRC20 tokens share (CRD ch.38 D38.6.4Q.1 status note,
//! items 1 to 4).
//!
//! Instances of the QTUM family whose every client input is exactly equal use one client. The registry owns none of
//! them: it keeps a weak reference per set of inputs, so a client lives as long as any instance holds it and closes
//! with the last holder. One build lock per set of inputs serializes the build or reuse; it is held, like the entries
//! of the maps, by weak reference, so the maps do not grow with the sets of inputs ever seen.
//!
//! Lock order: the activation locks of the instance (chapter 55 R55.1.12), then the build lock of the key, then the
//! maps of this registry (plain mutexes, never held across an await), and the coin registry only after both registry
//! locks are released. Nothing here calls into the coin registry.

use super::{ElectrumClient, ElectrumClientImpl, ElectrumRpcRequest};
use crate::node_url::{electrum_server_name, UNNAMED_SERVER};
use crate::utxo::ElectrumBuilderArgs;
use common::log::info;
use futures::lock::Mutex as AsyncMutex;
use serde_json::Value as Json;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

/// A server entry of an activation request, with every member the client uses, compared exactly.
#[derive(Clone, Debug, PartialEq)]
struct ServerKey {
    url: String,
    protocol: super::ElectrumProtocol,
    disable_cert_verification: bool,
    spki_sha256: Option<Json>,
    /// The bits of the timeout, so that the comparison is exact for every value.
    timeout_sec_bits: Option<u64>,
}

impl From<&ElectrumRpcRequest> for ServerKey {
    fn from(server: &ElectrumRpcRequest) -> ServerKey {
        ServerKey {
            url: server.url.clone(),
            protocol: server.protocol.clone(),
            disable_cert_verification: server.disable_cert_verification,
            spki_sha256: server.spki_sha256.clone(),
            timeout_sec_bits: server.timeout_sec.map(f64::to_bits),
        }
    }
}

/// Every input that shapes a shared client (D38.6.4Q.1 status note, item 1). Two instances share a client only when
/// their keys are equal. The `MM2.json` settings that shape a client are node-wide, so they are the same for every key
/// of one node and are not part of it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ClientKey {
    /// The platform: a token's `protocol.protocol_data.platform`, QTUM's own ticker.
    platform: String,
    /// The server list as the activation request gives it: in order, and per entry.
    servers: Vec<ServerKey>,
    min_connected: Option<usize>,
    max_connected: Option<usize>,
    listed_order_for_spends: bool,
    block_interval: Option<Duration>,
    /// `ElectrumBuilderArgs`: `spawn_ping`, `negotiate_version` and `collect_metrics`.
    args: (bool, bool, bool),
    /// The activation's `swap_evidence_node` (CRD ch.51 R100): two activations that differ only in it do not share.
    swap_evidence_node: Option<String>,
}

impl ClientKey {
    pub(crate) fn new(
        platform: &str,
        servers: &[ElectrumRpcRequest],
        min_connected: Option<usize>,
        max_connected: Option<usize>,
        listed_order_for_spends: bool,
        block_interval: Option<Duration>,
        args: &ElectrumBuilderArgs,
    ) -> ClientKey {
        ClientKey {
            platform: platform.to_owned(),
            servers: servers.iter().map(ServerKey::from).collect(),
            min_connected,
            max_connected,
            listed_order_for_spends,
            block_interval,
            args: (args.spawn_ping, args.negotiate_version, args.collect_metrics),
            swap_evidence_node: None,
        }
    }

    /// The key with the activation's `swap_evidence_node` (CRD ch.51 R100).
    pub(crate) fn with_swap_evidence_node(mut self, swap_evidence_node: Option<&str>) -> ClientKey {
        self.swap_evidence_node = swap_evidence_node.map(str::to_owned);
        self
    }

    /// The servers as a text may name them: `host:port` each, in the configured order, comma separated. Nothing else of
    /// an entry (scheme, user info, path, query) reaches the text (CRD ch.38 R38.6.9a).
    fn servers_text(&self) -> String {
        self.servers
            .iter()
            .map(|server| electrum_server_name(&server.url).unwrap_or_else(|| UNNAMED_SERVER.to_owned()))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Kept by a shared client, it logs the release of the client when the last holder lets go of it and the client is
/// dropped.
#[derive(Debug)]
pub(crate) struct ReleaseNote {
    platform: String,
    servers: String,
}

impl Drop for ReleaseNote {
    fn drop(&mut self) {
        info!(
            "Shared Electrum client of {} released, servers: {}",
            self.platform, self.servers
        );
    }
}

/// The shared Electrum clients of a node, by weak reference.
#[derive(Default)]
pub struct ElectrumClientRegistry {
    clients: Mutex<Vec<(ClientKey, Weak<ElectrumClientImpl>)>>,
    build_locks: Mutex<Vec<(ClientKey, Weak<AsyncMutex<()>>)>>,
}

impl ElectrumClientRegistry {
    /// The build lock of `key`, created when nobody holds or awaits one; the entries nobody holds are dropped.
    fn build_lock(&self, key: &ClientKey) -> Arc<AsyncMutex<()>> {
        let mut locks = self.build_locks.lock().unwrap_or_else(PoisonError::into_inner);
        locks.retain(|(_, lock)| lock.strong_count() > 0);
        if let Some(lock) = locks
            .iter()
            .find(|(known, _)| known == key)
            .and_then(|(_, lock)| lock.upgrade())
        {
            return lock;
        }
        let lock = Arc::new(AsyncMutex::new(()));
        locks.push((key.clone(), Arc::downgrade(&lock)));
        lock
    }

    /// The live client of `key`, if any; the entries of clients nobody holds any more are dropped.
    fn lookup(&self, key: &ClientKey) -> Option<Arc<ElectrumClientImpl>> {
        let mut clients = self.clients.lock().unwrap_or_else(PoisonError::into_inner);
        clients.retain(|(_, client)| client.strong_count() > 0);
        clients
            .iter()
            .find(|(known, _)| known == key)
            .and_then(|(_, client)| client.upgrade())
    }

    fn remember(&self, key: ClientKey, client: &Arc<ElectrumClientImpl>) {
        let mut clients = self.clients.lock().unwrap_or_else(PoisonError::into_inner);
        clients.retain(|(known, weak)| known != &key && weak.strong_count() > 0);
        clients.push((key, Arc::downgrade(client)));
    }

    /// Reuses the live client of `key` or builds one with `build`, under the build lock of the key.
    ///
    /// - A reused client is not probed again. While its servers disagree about the chain
    ///   ([`ElectrumClientImpl::chain_disagreement`]), the caller is refused with `disagreement(text)`, the error a
    ///   fresh build gives (R38.6.4o item 12), and holds no part of the client.
    /// - A build that failed is not remembered: the caller that waited for it builds from the start.
    ///
    /// One INFO line each says that the client was created or reused, by `instance`; the release is logged when the
    /// last holder drops the client (see [`ReleaseNote`]).
    pub(crate) async fn get_or_build<E, Fut>(
        &self,
        instance: &str,
        key: ClientKey,
        disagreement: impl FnOnce(String) -> E,
        build: impl FnOnce() -> Fut,
    ) -> Result<ElectrumClient, E>
    where
        Fut: Future<Output = Result<ElectrumClient, E>>,
    {
        let build_lock = self.build_lock(&key);
        let _building = build_lock.lock().await;
        if let Some(client) = self.lookup(&key) {
            return match client.chain_disagreement() {
                Some(text) => Err(disagreement(text)),
                None => {
                    info!(
                        "Shared Electrum client of {} reused by {}, servers: {}",
                        key.platform,
                        instance,
                        key.servers_text()
                    );
                    Ok(ElectrumClient(client))
                },
            };
        }
        let client = build().await?;
        self.remember(key.clone(), &client.0);
        let servers = key.servers_text();
        info!(
            "Shared Electrum client of {} created by {}, servers: {}",
            key.platform, instance, servers
        );
        client.0.note_release(ReleaseNote {
            platform: key.platform,
            servers,
        });
        Ok(client)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use common::block_on;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn server(url: &str) -> ElectrumRpcRequest { serde_json::from_value(serde_json::json!({ "url": url })).unwrap() }

    fn key_with(servers: &[ElectrumRpcRequest], args: ElectrumBuilderArgs) -> ClientKey {
        ClientKey::new("QTUM", servers, None, None, true, None, &args)
    }

    async fn built(builds: &AtomicUsize) -> Result<ElectrumClient, String> {
        builds.fetch_add(1, Ordering::SeqCst);
        Ok(ElectrumClient(Arc::new(ElectrumClientImpl::new(
            "QTUM".to_owned(),
            vec![],
        ))))
    }

    #[test]
    fn equal_keys_share_one_client_for_as_long_as_somebody_holds_it() {
        let registry = ElectrumClientRegistry::default();
        let builds = AtomicUsize::new(0);
        let servers = [server("127.0.0.1:1"), server("127.0.0.1:2")];
        let key = || key_with(&servers, ElectrumBuilderArgs::default());

        let first = block_on(registry.get_or_build("QTUM", key(), |text| text, || built(&builds))).unwrap();
        let second = block_on(registry.get_or_build("QTUM", key(), |text| text, || built(&builds))).unwrap();
        assert!(Arc::ptr_eq(&first.0, &second.0));
        assert_eq!(builds.load(Ordering::SeqCst), 1);

        // The registry owns none of them: with the last holder gone the entry is stale and a new build follows.
        drop((first, second));
        let third = block_on(registry.get_or_build("QTUM", key(), |text| text, || built(&builds))).unwrap();
        assert_eq!(builds.load(Ordering::SeqCst), 2);
        drop(third);
        assert!(registry.lookup(&key()).is_none());
        assert_eq!(
            registry.clients.lock().unwrap().len(),
            0,
            "a stale entry is pruned on lookup"
        );
    }

    /// CRD ch.51 R100: two activations that differ only in `swap_evidence_node` do not share a client.
    #[test]
    fn the_swap_evidence_node_tells_clients_apart() {
        let servers = [server("127.0.0.1:1"), server("127.0.0.1:2")];
        let key =
            |field: Option<&str>| key_with(&servers, ElectrumBuilderArgs::default()).with_swap_evidence_node(field);
        assert_eq!(key(None), key(None));
        assert_eq!(key(Some("127.0.0.1:1")), key(Some("127.0.0.1:1")));
        assert_ne!(key(None), key(Some("127.0.0.1:1")));
        assert_ne!(key(Some("127.0.0.1:1")), key(Some("127.0.0.1:2")));
    }

    #[test]
    fn every_input_of_the_key_tells_clients_apart() {
        let servers = [server("127.0.0.1:1"), server("127.0.0.1:2")];
        let base = ClientKey::new(
            "QTUM",
            &servers,
            None,
            None,
            true,
            None,
            &ElectrumBuilderArgs::default(),
        );
        let args = |spawn_ping, negotiate_version, collect_metrics| ElectrumBuilderArgs {
            spawn_ping,
            negotiate_version,
            collect_metrics,
        };
        let reversed = [server("127.0.0.1:2"), server("127.0.0.1:1")];
        let mut pinned = server("127.0.0.1:1");
        pinned.spki_sha256 = Some(serde_json::json!("00"));
        let mut slow = server("127.0.0.1:1");
        slow.timeout_sec = Some(5.);
        let mut insecure = server("127.0.0.1:1");
        insecure.disable_cert_verification = true;
        let mut ssl = server("127.0.0.1:1");
        ssl.protocol = super::super::ElectrumProtocol::SSL;
        let with_server = |entry: ElectrumRpcRequest| [entry, server("127.0.0.1:2")];

        let others = [
            ClientKey::new(
                "TQTUM",
                &servers,
                None,
                None,
                true,
                None,
                &ElectrumBuilderArgs::default(),
            ),
            ClientKey::new(
                "QTUM",
                &reversed,
                None,
                None,
                true,
                None,
                &ElectrumBuilderArgs::default(),
            ),
            ClientKey::new(
                "QTUM",
                &servers[..1],
                None,
                None,
                true,
                None,
                &ElectrumBuilderArgs::default(),
            ),
            ClientKey::new(
                "QTUM",
                &servers,
                Some(2),
                None,
                true,
                None,
                &ElectrumBuilderArgs::default(),
            ),
            ClientKey::new(
                "QTUM",
                &servers,
                None,
                Some(2),
                true,
                None,
                &ElectrumBuilderArgs::default(),
            ),
            ClientKey::new(
                "QTUM",
                &servers,
                None,
                None,
                false,
                None,
                &ElectrumBuilderArgs::default(),
            ),
            ClientKey::new(
                "QTUM",
                &servers,
                None,
                None,
                true,
                Some(Duration::from_secs(60)),
                &ElectrumBuilderArgs::default(),
            ),
            ClientKey::new("QTUM", &servers, None, None, true, None, &args(false, true, true)),
            ClientKey::new("QTUM", &servers, None, None, true, None, &args(true, false, true)),
            ClientKey::new("QTUM", &servers, None, None, true, None, &args(true, true, false)),
            ClientKey::new(
                "QTUM",
                &with_server(pinned),
                None,
                None,
                true,
                None,
                &ElectrumBuilderArgs::default(),
            ),
            ClientKey::new(
                "QTUM",
                &with_server(slow),
                None,
                None,
                true,
                None,
                &ElectrumBuilderArgs::default(),
            ),
            ClientKey::new(
                "QTUM",
                &with_server(insecure),
                None,
                None,
                true,
                None,
                &ElectrumBuilderArgs::default(),
            ),
            ClientKey::new(
                "QTUM",
                &with_server(ssl),
                None,
                None,
                true,
                None,
                &ElectrumBuilderArgs::default(),
            ),
        ];
        for (index, other) in others.iter().enumerate() {
            assert_ne!(&base, other, "input {index} does not tell the clients apart");
        }
        assert_eq!(
            base,
            ClientKey::new(
                "QTUM",
                &servers,
                None,
                None,
                true,
                None,
                &ElectrumBuilderArgs::default()
            )
        );
    }

    /// Test 7: different `ElectrumBuilderArgs` give two clients.
    #[test]
    fn different_builder_arguments_give_two_clients() {
        let registry = ElectrumClientRegistry::default();
        let builds = AtomicUsize::new(0);
        let servers = [server("127.0.0.1:1")];
        let quiet = ElectrumBuilderArgs {
            spawn_ping: false,
            negotiate_version: true,
            collect_metrics: false,
        };
        let first = block_on(registry.get_or_build(
            "QTUM",
            key_with(&servers, ElectrumBuilderArgs::default()),
            |text| text,
            || built(&builds),
        ))
        .unwrap();
        let second =
            block_on(registry.get_or_build("QTUM", key_with(&servers, quiet), |text| text, || built(&builds))).unwrap();
        assert!(!Arc::ptr_eq(&first.0, &second.0));
        assert_eq!(builds.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_failed_build_is_not_remembered_and_its_lock_is_not_kept() {
        let registry = ElectrumClientRegistry::default();
        let servers = [server("127.0.0.1:1")];
        let key = || key_with(&servers, ElectrumBuilderArgs::default());

        let failed = block_on(registry.get_or_build(
            "QTUM",
            key(),
            |text| text,
            || async { Err::<ElectrumClient, _>("no server answered".to_owned()) },
        ));
        assert_eq!(failed.unwrap_err(), "no server answered");
        assert!(registry.lookup(&key()).is_none());
        assert_eq!(registry.build_locks.lock().unwrap().len(), 1);

        // The next instance builds from the start, and the lock entry nobody holds is dropped meanwhile.
        let builds = AtomicUsize::new(0);
        let client = block_on(registry.get_or_build("QTUM", key(), |text| text, || built(&builds))).unwrap();
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        assert_eq!(registry.build_locks.lock().unwrap().len(), 1);
        drop(client);
    }

    /// A server entry with user info, a path and a query is named `host:port` in the INFO lines of the client, and
    /// nothing else of it.
    #[test]
    fn the_info_lines_name_each_server_as_host_and_port_only() {
        use crate::eth::alloy_compat::transport::test_support::{captured_log_lines, install_log_capture};
        let capturing = install_log_capture();
        let registry = ElectrumClientRegistry::default();
        let builds = AtomicUsize::new(0);
        let servers = [
            server("ssl://user:pw-placeholder@info-lines-test.invalid:50002/some/path?token=abc#frag"),
            server("info-lines-test.invalid:50001"),
            server("pw-placeholder"),
        ];
        let key = || key_with(&servers, ElectrumBuilderArgs::default());
        let expected = "info-lines-test.invalid:50002, info-lines-test.invalid:50001, <invalid server address>";

        let first = block_on(registry.get_or_build("QRC20", key(), |text| text, || built(&builds))).unwrap();
        let second = block_on(registry.get_or_build("QRC20B", key(), |text| text, || built(&builds))).unwrap();
        drop((first, second));
        if capturing {
            let lines: Vec<String> = captured_log_lines()
                .into_iter()
                .filter(|line| line.contains("info-lines-test.invalid"))
                .collect();
            let wanted = [
                format!("INFO Shared Electrum client of QTUM created by QRC20, servers: {expected}"),
                format!("INFO Shared Electrum client of QTUM reused by QRC20B, servers: {expected}"),
                format!("INFO Shared Electrum client of QTUM released, servers: {expected}"),
            ];
            assert!(
                lines == wanted,
                "the INFO lines of the shared client are not the expected three"
            );
            for line in captured_log_lines()
                .iter()
                .filter(|line| line.contains("Shared Electrum client"))
            {
                assert!(
                    !["pw-placeholder", "token", "some/path", "frag", "user", "ssl://"]
                        .iter()
                        .any(|secret| line.contains(secret) && line.contains("info-lines-test")),
                    "an INFO line of the shared client carries more than host:port"
                );
            }
        }
    }
}
