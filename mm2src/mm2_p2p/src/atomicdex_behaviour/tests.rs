use super::{spawn_gossipsub, AdexBehaviourCmd, AdexBehaviourEvent, AdexResponse, NodeType, RelayAddress};
use async_std::task::spawn;
use futures::channel::{mpsc, oneshot};
use futures::future::AbortHandle;
use futures::{Future, SinkExt, StreamExt};
use libp2p::PeerId;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

static TEST_LISTEN_PORT: AtomicU64 = AtomicU64::new(1);

fn next_port() -> u64 { TEST_LISTEN_PORT.fetch_add(1, Ordering::Relaxed) }

fn spawn_boxed(fut: Box<dyn Future<Output = ()> + Send + Unpin + 'static>) { spawn(fut); }

struct Node {
    peer_id: PeerId,
    cmd_tx: mpsc::Sender<AdexBehaviourCmd>,
    abort_handle: AbortHandle,
}

impl Node {
    async fn spawn<F>(port: u64, seednodes: Vec<u64>, on_event: F) -> Node
    where
        F: Fn(mpsc::Sender<AdexBehaviourCmd>, AdexBehaviourEvent) + Send + 'static,
    {
        Node::spawn_with_type(NodeType::RelayInMemory { port }, seednodes, on_event).await
    }

    async fn spawn_light<F>(seednodes: Vec<u64>, on_event: F) -> Node
    where
        F: Fn(mpsc::Sender<AdexBehaviourCmd>, AdexBehaviourEvent) + Send + 'static,
    {
        Node::spawn_with_type(NodeType::LightInMemory, seednodes, on_event).await
    }

    async fn spawn_with_type<F>(node_type: NodeType, seednodes: Vec<u64>, on_event: F) -> Node
    where
        F: Fn(mpsc::Sender<AdexBehaviourCmd>, AdexBehaviourEvent) + Send + 'static,
    {
        let seednodes = seednodes.into_iter().map(RelayAddress::Memory).collect();
        let (cmd_tx, mut event_rx, peer_id, abort_handle) =
            spawn_gossipsub(None, spawn_boxed, seednodes, node_type, |_| {})
                .await
                .expect("Error spawning AdexBehaviour");

        // spawn a response future
        let cmd_tx_fut = cmd_tx.clone();
        spawn(async move {
            loop {
                let cmd_tx_fut = cmd_tx_fut.clone();
                match event_rx.next().await {
                    Some(r) => on_event(cmd_tx_fut, r),
                    _ => {
                        println!("Finish response future");
                        break;
                    },
                }
            }
        });

        Node {
            peer_id,
            cmd_tx,
            abort_handle,
        }
    }

    async fn send_cmd(&mut self, cmd: AdexBehaviourCmd) { self.cmd_tx.send(cmd).await.unwrap(); }

    /// Stops the underlying libp2p swarm driver. The in-memory transport
    /// observes the aborted future as a disconnect on the peer side.
    fn abort(&self) { self.abort_handle.abort(); }

    /// Poll `GetPeersInfo` until the connected peer count reaches exactly
    /// `number`. Panics after `attempts` 500 ms retries.
    async fn wait_peers_exact(&mut self, number: usize, attempts: usize) {
        for _ in 0..attempts {
            let (tx, rx) = oneshot::channel();
            self.cmd_tx
                .send(AdexBehaviourCmd::GetPeersInfo { result_tx: tx })
                .await
                .unwrap();
            let map = rx.await.unwrap();
            if map.len() == number {
                return;
            }
            async_std::task::sleep(Duration::from_millis(500)).await;
        }
        panic!("wait_peers_exact({number}) attempts exceeded");
    }

    async fn wait_peers(&mut self, number: usize) {
        let mut attempts = 0;
        loop {
            let (tx, rx) = oneshot::channel();
            self.cmd_tx
                .send(AdexBehaviourCmd::GetPeersInfo { result_tx: tx })
                .await
                .unwrap();
            match rx.await {
                Ok(map) => {
                    if map.len() >= number {
                        return;
                    }
                    async_std::task::sleep(Duration::from_millis(500)).await;
                },
                Err(e) => panic!("{}", e),
            }
            attempts += 1;
            if attempts >= 10 {
                panic!("wait_peers {} attempts exceeded", attempts);
            }
        }
    }
}

#[tokio::test]
async fn test_request_response_ok() {
    let _ = env_logger::try_init();

    let request_received = Arc::new(AtomicBool::new(false));
    let request_received_cpy = request_received.clone();

    let node1_port = next_port();
    let node1 = Node::spawn(node1_port, vec![], move |mut cmd_tx, event| {
        let (request, response_channel) = match event {
            AdexBehaviourEvent::PeerRequest {
                request,
                response_channel,
                ..
            } => (request, response_channel),
            _ => return,
        };

        request_received_cpy.store(true, Ordering::Relaxed);
        assert_eq!(request, b"test request");

        let res = AdexResponse::Ok {
            response: b"test response".to_vec(),
        };
        cmd_tx
            .try_send(AdexBehaviourCmd::SendResponse { res, response_channel })
            .unwrap();
    })
    .await;

    let mut node2 = Node::spawn(next_port(), vec![node1_port], |_, _| ()).await;

    node2.wait_peers(1).await;

    let (response_tx, response_rx) = oneshot::channel();
    node2
        .send_cmd(AdexBehaviourCmd::RequestAnyRelay {
            req: b"test request".to_vec(),
            response_tx,
        })
        .await;

    let response = response_rx.await.unwrap();
    assert_eq!(response, Some((node1.peer_id, b"test response".to_vec())));

    assert!(request_received.load(Ordering::Relaxed));
}

#[tokio::test]
async fn test_request_response_ok_three_peers() {
    let _ = env_logger::try_init();

    #[derive(Default)]
    struct RequestHandler {
        requests: u8,
    }

    impl RequestHandler {
        fn handle(&mut self, mut cmd_tx: mpsc::Sender<AdexBehaviourCmd>, event: AdexBehaviourEvent) {
            let (request, response_channel) = match event {
                AdexBehaviourEvent::PeerRequest {
                    request,
                    response_channel,
                    ..
                } => (request, response_channel),
                _ => return,
            };

            self.requests += 1;

            assert_eq!(request, b"test request");

            // the first time we should respond the none
            if self.requests == 1 {
                let res = AdexResponse::None;
                cmd_tx
                    .try_send(AdexBehaviourCmd::SendResponse { res, response_channel })
                    .unwrap();
                return;
            }

            // the second time we should respond an error
            if self.requests == 2 {
                let res = AdexResponse::Err {
                    error: "test error".into(),
                };
                cmd_tx
                    .try_send(AdexBehaviourCmd::SendResponse { res, response_channel })
                    .unwrap();
                return;
            }

            // the third time we should respond an ok
            if self.requests == 3 {
                let res = AdexResponse::Ok {
                    response: format!("success {} request", self.requests).as_bytes().to_vec(),
                };
                cmd_tx
                    .try_send(AdexBehaviourCmd::SendResponse { res, response_channel })
                    .unwrap();
                return;
            }

            panic!("Request received more than 3 times");
        }
    }

    let request_handler = Arc::new(Mutex::new(RequestHandler::default()));

    let mut receivers = Vec::new();
    for _ in 0..3 {
        let handler = request_handler.clone();
        let receiver_port = next_port();
        let receiver = Node::spawn(receiver_port, vec![], move |cmd_tx, event| {
            let mut handler = handler.lock().unwrap();
            handler.handle(cmd_tx, event)
        })
        .await;
        receivers.push((receiver_port, receiver));
    }

    let mut sender = Node::spawn(
        next_port(),
        receivers.iter().map(|(port, _)| *port).collect(),
        |_, _| (),
    )
    .await;

    sender.wait_peers(3).await;

    let (response_tx, response_rx) = oneshot::channel();
    sender
        .send_cmd(AdexBehaviourCmd::RequestAnyRelay {
            req: b"test request".to_vec(),
            response_tx,
        })
        .await;

    let (_peer_id, res) = response_rx.await.unwrap().unwrap();
    assert_eq!(res, b"success 3 request".to_vec());
}

#[tokio::test]
async fn test_request_response_none() {
    let _ = env_logger::try_init();

    let request_received = Arc::new(AtomicBool::new(false));
    let request_received_cpy = request_received.clone();

    let node1_port = next_port();
    let _node1 = Node::spawn(node1_port, vec![], move |mut cmd_tx, event| {
        let (request, response_channel) = match event {
            AdexBehaviourEvent::PeerRequest {
                request,
                response_channel,
                ..
            } => (request, response_channel),
            _ => return,
        };

        request_received_cpy.store(true, Ordering::Relaxed);
        assert_eq!(request, b"test request");

        let res = AdexResponse::None;
        cmd_tx
            .try_send(AdexBehaviourCmd::SendResponse { res, response_channel })
            .unwrap();
    })
    .await;

    let mut node2 = Node::spawn(next_port(), vec![node1_port], |_, _| ()).await;

    node2.wait_peers(1).await;

    let (response_tx, response_rx) = oneshot::channel();
    node2
        .send_cmd(AdexBehaviourCmd::RequestAnyRelay {
            req: b"test request".to_vec(),
            response_tx,
        })
        .await;

    assert_eq!(response_rx.await.unwrap(), None);
    assert!(request_received.load(Ordering::Relaxed));
}

#[tokio::test]
async fn test_request_peers_ok_three_peers() {
    let _ = env_logger::try_init();

    let receiver1_port = next_port();
    let receiver1 = Node::spawn(receiver1_port, vec![], move |mut cmd_tx, event| {
        let (request, response_channel) = match event {
            AdexBehaviourEvent::PeerRequest {
                request,
                response_channel,
                ..
            } => (request, response_channel),
            _ => return,
        };

        assert_eq!(request, b"test request");

        let res = AdexResponse::None;
        cmd_tx
            .try_send(AdexBehaviourCmd::SendResponse { res, response_channel })
            .unwrap();
    })
    .await;

    let receiver2_port = next_port();
    let receiver2 = Node::spawn(receiver2_port, vec![], move |mut cmd_tx, event| {
        let (request, response_channel) = match event {
            AdexBehaviourEvent::PeerRequest {
                request,
                response_channel,
                ..
            } => (request, response_channel),
            _ => return,
        };

        assert_eq!(request, b"test request");

        let res = AdexResponse::Err {
            error: "test error".into(),
        };
        cmd_tx
            .try_send(AdexBehaviourCmd::SendResponse { res, response_channel })
            .unwrap();
    })
    .await;

    let receiver3_port = next_port();
    let receiver3 = Node::spawn(receiver3_port, vec![], move |mut cmd_tx, event| {
        let (request, response_channel) = match event {
            AdexBehaviourEvent::PeerRequest {
                request,
                response_channel,
                ..
            } => (request, response_channel),
            _ => return,
        };

        assert_eq!(request, b"test request");

        let res = AdexResponse::Ok {
            response: b"test response".to_vec(),
        };
        cmd_tx
            .try_send(AdexBehaviourCmd::SendResponse { res, response_channel })
            .unwrap();
    })
    .await;
    let mut sender = Node::spawn(
        next_port(),
        vec![receiver1_port, receiver2_port, receiver3_port],
        |_, _| (),
    )
    .await;

    sender.wait_peers(3).await;

    let (response_tx, response_rx) = oneshot::channel();
    sender
        .send_cmd(AdexBehaviourCmd::RequestRelays {
            req: b"test request".to_vec(),
            response_tx,
        })
        .await;

    let mut expected = vec![
        (receiver1.peer_id, AdexResponse::None),
        (receiver2.peer_id, AdexResponse::Err {
            error: "test error".into(),
        }),
        (receiver3.peer_id, AdexResponse::Ok {
            response: b"test response".to_vec(),
        }),
    ];
    expected.sort_by(|x, y| x.0.cmp(&y.0));

    let mut responses = response_rx.await.unwrap();
    responses.sort_by(|x, y| x.0.cmp(&y.0));
    assert_eq!(responses, expected);
}

// ── TP3: P2P / Gossipsub Smoke Tests ────────────────────────────────────
//
// These tests exercise the full publish/subscribe path of the
// `AtomicDexBehaviour` over the in-memory libp2p transport. They are
// pure-Rust replacements for the multi-process smoke tests originally
// scoped under TP3, covering the same propagation invariants without
// spawning external KDF binaries.
//
// In this fork relay nodes (`i_am_relay = true`) treat themselves as
// implicitly subscribed to every topic and the local `subscribe()` call
// becomes a no-op. To exercise real SUBSCRIBE / publish propagation we
// therefore use light clients (`NodeType::LightInMemory`) seeded to a
// relay backbone.

/// A light client subscribing to a topic must surface as a `Subscribed`
/// event on the connected relay (the relay is the one that receives the
/// remote SUBSCRIBE control message).
#[tokio::test]
async fn test_subscribe_propagates_to_remote_peer() {
    let _ = env_logger::try_init();

    let topic = "tp3-smoke-subscribe".to_owned();

    let relay_subs: Arc<Mutex<Vec<libp2p::gossipsub::TopicHash>>> = Arc::new(Mutex::new(Vec::new()));
    let relay_subs_cpy = relay_subs.clone();

    let relay_port = next_port();
    let mut relay = Node::spawn(relay_port, vec![], move |_cmd_tx, event| {
        if let AdexBehaviourEvent::Subscribed { topic, .. } = event {
            relay_subs_cpy.lock().unwrap().push(topic);
        }
    })
    .await;

    let mut subscriber = Node::spawn_light(vec![relay_port], |_, _| ()).await;
    subscriber.wait_peers(1).await;

    subscriber
        .send_cmd(AdexBehaviourCmd::Subscribe { topic: topic.clone() })
        .await;

    let mut got = false;
    for _ in 0..30 {
        if relay_subs.lock().unwrap().iter().any(|t| t.as_str() == topic) {
            got = true;
            break;
        }
        async_std::task::sleep(Duration::from_millis(500)).await;
    }
    let observed = relay_subs.lock().unwrap().clone();
    assert!(got, "relay never observed Subscribed event for {topic}: {observed:?}",);

    let _ = &mut relay;
}

/// Light publisher → relay → light subscriber. The subscriber must
/// receive a `Message` event for the payload broadcast by the publisher
/// after both lights have joined the topic mesh through the relay.
#[tokio::test]
async fn test_publish_reaches_direct_subscriber() {
    let _ = env_logger::try_init();

    let topic = "tp3-smoke-publish-direct".to_owned();
    let payload = b"hello from publisher".to_vec();

    let received: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let received_cpy = received.clone();

    let relay_port = next_port();
    let mut _relay = Node::spawn(relay_port, vec![], |mut cmd_tx, event| {
        if let AdexBehaviourEvent::Message(peer_id, message_id, _) = event {
            let _ = cmd_tx.try_send(AdexBehaviourCmd::PropagateMessage {
                message_id,
                propagation_source: peer_id,
            });
        }
    })
    .await;

    let mut publisher = Node::spawn_light(vec![relay_port], |_, _| ()).await;
    let mut subscriber = Node::spawn_light(vec![relay_port], move |_cmd_tx, event| {
        if let AdexBehaviourEvent::Message(_, _, msg) = event {
            received_cpy.lock().unwrap().push(msg.data);
        }
    })
    .await;

    publisher.wait_peers(1).await;
    subscriber.wait_peers(1).await;

    publisher
        .send_cmd(AdexBehaviourCmd::Subscribe { topic: topic.clone() })
        .await;
    subscriber
        .send_cmd(AdexBehaviourCmd::Subscribe { topic: topic.clone() })
        .await;

    // Allow gossipsub heartbeats (initial delay 5 s, interval 1 s) to
    // exchange SUBSCRIBE control messages and form the relay mesh that
    // carries published payloads from the light publisher to the relay.
    async_std::task::sleep(Duration::from_secs(15)).await;

    publisher
        .send_cmd(AdexBehaviourCmd::PublishMsg {
            topics: vec![topic.clone()],
            msg: payload.clone(),
        })
        .await;

    for _ in 0..30 {
        if !received.lock().unwrap().is_empty() {
            break;
        }
        async_std::task::sleep(Duration::from_millis(500)).await;
    }

    let observed = received.lock().unwrap().clone();
    assert!(
        observed.contains(&payload),
        "subscriber never received published payload: {observed:?}",
    );
}

/// Light publisher → relay1 → relay2 → light subscriber. The subscriber
/// is two hops away from the publisher, so the message must traverse the
/// relay-to-relay mesh to reach it.
#[tokio::test]
async fn test_publish_reaches_subscriber_via_relay() {
    let _ = env_logger::try_init();

    let topic = "tp3-smoke-publish-relay".to_owned();
    let payload = b"hello from publisher via relay".to_vec();

    let received: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let received_cpy = received.clone();

    let relay1_port = next_port();
    let mut _relay1 = Node::spawn(relay1_port, vec![], |mut cmd_tx, event| {
        if let AdexBehaviourEvent::Message(peer_id, message_id, _) = event {
            let _ = cmd_tx.try_send(AdexBehaviourCmd::PropagateMessage {
                message_id,
                propagation_source: peer_id,
            });
        }
    })
    .await;

    let relay2_port = next_port();
    let mut relay2 = Node::spawn(relay2_port, vec![relay1_port], |mut cmd_tx, event| {
        if let AdexBehaviourEvent::Message(peer_id, message_id, _) = event {
            let _ = cmd_tx.try_send(AdexBehaviourCmd::PropagateMessage {
                message_id,
                propagation_source: peer_id,
            });
        }
    })
    .await;
    relay2.wait_peers(1).await;

    let mut publisher = Node::spawn_light(vec![relay1_port], |_, _| ()).await;
    let mut subscriber = Node::spawn_light(vec![relay2_port], move |_cmd_tx, event| {
        if let AdexBehaviourEvent::Message(_, _, msg) = event {
            received_cpy.lock().unwrap().push(msg.data);
        }
    })
    .await;

    publisher.wait_peers(1).await;
    subscriber.wait_peers(1).await;

    publisher
        .send_cmd(AdexBehaviourCmd::Subscribe { topic: topic.clone() })
        .await;
    subscriber
        .send_cmd(AdexBehaviourCmd::Subscribe { topic: topic.clone() })
        .await;

    async_std::task::sleep(Duration::from_secs(20)).await;

    publisher
        .send_cmd(AdexBehaviourCmd::PublishMsg {
            topics: vec![topic.clone()],
            msg: payload.clone(),
        })
        .await;

    for _ in 0..40 {
        if !received.lock().unwrap().is_empty() {
            break;
        }
        async_std::task::sleep(Duration::from_millis(500)).await;
    }

    let observed = received.lock().unwrap().clone();
    assert!(
        observed.contains(&payload),
        "relayed subscriber never received payload: {observed:?}",
    );
}

/// Disconnection edge case: when a relay's swarm driver is aborted the
/// light clients connected to it must observe the peer drop. Exercises
/// the libp2p-level teardown path (in-memory channel close propagating
/// to `inject_disconnected` on the remote side).
#[tokio::test]
async fn test_light_client_observes_relay_disconnect() {
    let _ = env_logger::try_init();

    let relay_port = next_port();
    let relay = Node::spawn(relay_port, vec![], |_, _| ()).await;

    let mut light_a = Node::spawn_light(vec![relay_port], |_, _| ()).await;
    let mut light_b = Node::spawn_light(vec![relay_port], |_, _| ()).await;

    light_a.wait_peers(1).await;
    light_b.wait_peers(1).await;

    // Tear down the relay swarm. Both light clients should drop their
    // only peer within a few poll iterations.
    relay.abort();

    light_a.wait_peers_exact(0, 30).await;
    light_b.wait_peers_exact(0, 30).await;
}

/// Watcher-node smoke test: a third light client passively subscribed to
/// the same topic as the swap participants must receive every published
/// message. Models the real-world watcher node that monitors
/// counterparty swap traffic without participating in the swap itself.
///
/// Topology: publisher (light) → relay → { participant (light), watcher (light) }
/// Both subscribers must observe the same payloads in order.
#[tokio::test]
async fn test_watcher_node_observes_swap_traffic() {
    let _ = env_logger::try_init();

    let topic = "tp3-smoke-watcher".to_owned();
    let payload_a = b"swap-msg-1".to_vec();
    let payload_b = b"swap-msg-2".to_vec();

    let participant_rx: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let participant_rx_cpy = participant_rx.clone();

    let watcher_rx: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let watcher_rx_cpy = watcher_rx.clone();

    let relay_port = next_port();
    let mut relay = Node::spawn(relay_port, vec![], |mut cmd_tx, event| {
        if let AdexBehaviourEvent::Message(peer_id, message_id, _) = event {
            let _ = cmd_tx.try_send(AdexBehaviourCmd::PropagateMessage {
                message_id,
                propagation_source: peer_id,
            });
        }
    })
    .await;

    let mut publisher = Node::spawn_light(vec![relay_port], |_, _| ()).await;
    let mut participant = Node::spawn_light(vec![relay_port], move |_cmd_tx, event| {
        if let AdexBehaviourEvent::Message(_, _, msg) = event {
            participant_rx_cpy.lock().unwrap().push(msg.data);
        }
    })
    .await;
    let mut watcher = Node::spawn_light(vec![relay_port], move |_cmd_tx, event| {
        if let AdexBehaviourEvent::Message(_, _, msg) = event {
            watcher_rx_cpy.lock().unwrap().push(msg.data);
        }
    })
    .await;

    relay.wait_peers(3).await;
    publisher.wait_peers(1).await;
    participant.wait_peers(1).await;
    watcher.wait_peers(1).await;

    publisher
        .send_cmd(AdexBehaviourCmd::Subscribe { topic: topic.clone() })
        .await;
    participant
        .send_cmd(AdexBehaviourCmd::Subscribe { topic: topic.clone() })
        .await;
    watcher
        .send_cmd(AdexBehaviourCmd::Subscribe { topic: topic.clone() })
        .await;

    // Allow the gossipsub mesh to converge across all three subscribers.
    async_std::task::sleep(Duration::from_secs(20)).await;

    publisher
        .send_cmd(AdexBehaviourCmd::PublishMsg {
            topics: vec![topic.clone()],
            msg: payload_a.clone(),
        })
        .await;
    publisher
        .send_cmd(AdexBehaviourCmd::PublishMsg {
            topics: vec![topic.clone()],
            msg: payload_b.clone(),
        })
        .await;

    for _ in 0..40 {
        let p_done = participant_rx.lock().unwrap().len() >= 2;
        let w_done = watcher_rx.lock().unwrap().len() >= 2;
        if p_done && w_done {
            break;
        }
        async_std::task::sleep(Duration::from_millis(500)).await;
    }

    let p_observed = participant_rx.lock().unwrap().clone();
    let w_observed = watcher_rx.lock().unwrap().clone();

    assert!(
        p_observed.contains(&payload_a) && p_observed.contains(&payload_b),
        "swap participant missed payloads: {p_observed:?}",
    );
    assert!(
        w_observed.contains(&payload_a) && w_observed.contains(&payload_b),
        "watcher node missed payloads: {w_observed:?}",
    );
}
