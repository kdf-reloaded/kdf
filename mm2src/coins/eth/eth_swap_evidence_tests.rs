//! The legacy EVM swap reads under the swap-evidence rule (CRD ch.51 §51.6G R100, T42, T43).
//!
//! Every node is played by the stub sender of the transport tests, so nothing reaches a network. Each node has its
//! own script (tip, chain time, the contract's state of the payment by block, the claim or refund log), which a test
//! may change between two attempts. The coin's failover provider is a stub too, and no agreed read may reach it.

use super::alloy_compat::transport::test_support::{captured_log_lines, error_body, install_log_capture, result_body,
                                                   stub_provider, Call, Reply, StubSender};
use super::eth_tests::eth_coin_for_test_with_providers;
use super::*;
use crate::swap_evidence::{is_undetermined, swap_evidence_policy_from};
use crate::{CanRefundHtlc, SwapEvidencePolicy};
use common::block_on;
use crypto::secret_hash_algo::SecretHashAlgo;

const A: &str = "http://a.example:8545";
/// A second port of the host of [`A`].
const A2: &str = "http://a.example:9545";
const B: &str = "http://b.example:8545";
const C: &str = "http://c.example:8545";
const FAILOVER: &str = "http://failover.example:8545";

const TIME_LOCK: u32 = 1_700_000_000;
const SECRET: [u8; 32] = [0x44; 32];
const OTHER_SECRET: [u8; 32] = [0x45; 32];
const COIN_KEY: &str = "809465b17d0a4ddb3e4c69e8f23c2cabad868f51f8bed5c765ad1d6516c3306f";

/// The timestamp of every node's latest block unless a test sets another.
const TIP_TIME: u64 = 1_700_000_000;

const SENT: u8 = 1;
const SPENT: u8 = 2;
const REFUNDED: u8 = 3;

fn secret_hash() -> Vec<u8> { SecretHashAlgo::DHASH160.hash_secret(&SECRET) }

fn paid_wei() -> U256 { U256::from(1_000_000_000_000_000_000u64) }

/// The log of a claim or a refund as a node serves it.
#[derive(Clone)]
struct Spend {
    event: &'static str,
    block: u64,
    tx: SignedEthTx,
    data: Vec<u8>,
}

/// What one node answers.
#[derive(Clone, Default)]
struct Node {
    /// The node refuses every connection.
    down: bool,
    tip: u64,
    /// The timestamp of the node's latest block.
    timestamp: u64,
    /// `(from block, state)`: the contract's state of the payment from that block on; uninitialized before.
    states: Vec<(u64, u8)>,
    spend: Option<Spend>,
    /// Whether the node returns the payment transaction.
    knows_payment: bool,
}

#[derive(Default)]
struct World {
    nodes: Mutex<HashMap<String, Node>>,
    payment: Mutex<Option<SignedEthTx>>,
    /// `(host, method, params)` of every request, in arrival order.
    seen: Mutex<Vec<(String, String, Json)>>,
}

fn event_topic(event: &str) -> String { format!("{:#x}", SWAP_CONTRACT.event(event).unwrap().signature()) }

/// The `eth_getTransactionByHash` result a node reports for a legacy transaction.
fn rpc_tx(tx: &SignedEthTx) -> Json {
    let to = match &tx.action {
        Action::Call(addr) => json!(format!("{:#x}", addr)),
        Action::Create => Json::Null,
    };
    let gas_price = tx.gas_price().expect("a legacy transaction");
    json!({
        "type": "0x0",
        "hash": format!("{:#x}", tx.tx_hash()),
        "nonce": format!("{:#x}", tx.nonce),
        "blockHash": format!("0x{}", "22".repeat(32)),
        "blockNumber": "0x5a",
        "transactionIndex": "0x0",
        "from": format!("{:#x}", tx.sender()),
        "to": to,
        "value": format!("{:#x}", tx.value),
        "gas": format!("{:#x}", tx.gas),
        "gasPrice": format!("{:#x}", gas_price),
        "input": format!("0x{}", hex::encode(&tx.data)),
        "v": format!("{:#x}", tx.v),
        "r": format!("{:#x}", tx.r),
        "s": format!("{:#x}", tx.s),
    })
}

/// The `eth_getBlockByNumber` result of an empty block with the given height and timestamp.
fn rpc_block(number: u64, timestamp: u64) -> Json {
    json!({
        "hash": format!("0x{}", "11".repeat(32)),
        "parentHash": format!("0x{}", "22".repeat(32)),
        "sha3Uncles": format!("0x{}", "33".repeat(32)),
        "miner": format!("0x{}", "00".repeat(20)),
        "stateRoot": format!("0x{}", "44".repeat(32)),
        "transactionsRoot": format!("0x{}", "55".repeat(32)),
        "receiptsRoot": format!("0x{}", "66".repeat(32)),
        "logsBloom": format!("0x{}", "00".repeat(256)),
        "difficulty": "0x0",
        "number": format!("{:#x}", number),
        "gasLimit": "0x1c9c380",
        "gasUsed": "0x0",
        "timestamp": format!("{:#x}", timestamp),
        "extraData": "0x",
        "mixHash": format!("0x{}", "77".repeat(32)),
        "nonce": "0x0000000000000000",
        "transactions": [],
        "uncles": [],
    })
}

fn block_of(value: &Json) -> u64 { u64::from_str_radix(value.as_str().unwrap().trim_start_matches("0x"), 16).unwrap() }

fn answer(world: &World, call: &Call) -> Reply {
    let params = call.request["params"].clone();
    world
        .seen
        .lock()
        .unwrap()
        .push((call.host.clone(), call.method.clone(), params.clone()));
    let node = match world.nodes.lock().unwrap().get(&call.host) {
        Some(node) => node.clone(),
        None => panic!("no script for the node {}", call.host),
    };
    if node.down {
        return Reply::Fail("connection refused".to_owned());
    }
    let payment = world.payment.lock().unwrap().clone().expect("the payment is set");
    let result = match call.method.as_str() {
        "eth_call" => {
            let block = block_of(&params[1]);
            if block > node.tip {
                return Reply::Body(error_body(&call.request, -32000, "header not found"));
            }
            let state = node
                .states
                .iter()
                .rev()
                .find(|(from, _)| *from <= block)
                .map_or(0, |(_, state)| *state);
            // `payments(id)` returns (bytes20 paymentHash, uint64 lockTime, uint8 state): three words.
            json!(format!("0x{}{:02x}", "00".repeat(95), state))
        },
        "eth_getTransactionByHash" => {
            let asked = params[0].as_str().unwrap();
            match &node.spend {
                _ if node.knows_payment && asked == format!("{:#x}", payment.tx_hash()) => rpc_tx(&payment),
                Some(spend) if asked == format!("{:#x}", spend.tx.tx_hash()) => rpc_tx(&spend.tx),
                _ => Json::Null,
            }
        },
        // The node has no receipt of the payment: the log search then starts at the caller's start block.
        "eth_getTransactionReceipt" => Json::Null,
        "eth_getLogs" => {
            let filter = &params[0];
            let (from, to) = (block_of(&filter["fromBlock"]), block_of(&filter["toBlock"]));
            let asked_topic = filter["topics"][0].to_string();
            match &node.spend {
                Some(spend)
                    if asked_topic.contains(&event_topic(spend.event))
                        && (from..=to).contains(&spend.block)
                        && spend.block <= node.tip =>
                {
                    json!([{
                        "address": format!("{:#x}", Address::from_str("7Bc1bBDD6A0a722fC9bffC49c921B685ECB84b94").unwrap()),
                        "topics": [event_topic(spend.event)],
                        "data": format!("0x{}", hex::encode(&spend.data)),
                        "blockHash": format!("0x{}", "22".repeat(32)),
                        "blockNumber": format!("{:#x}", spend.block),
                        "transactionHash": format!("{:#x}", spend.tx.tx_hash()),
                        "transactionIndex": "0x0",
                        "logIndex": "0x0",
                        "removed": false,
                    }])
                },
                _ => json!([]),
            }
        },
        "eth_getBlockByNumber" => {
            assert_eq!(params[0], json!("latest"), "only the latest block is read");
            rpc_block(node.tip, node.timestamp)
        },
        // The readiness probe before a swap starts.
        "eth_blockNumber" => json!(format!("{:#x}", node.tip)),
        other => panic!("the scripted node does not serve `{other}`"),
    };
    Reply::Body(result_body(&call.request, &result.to_string()))
}

struct Fixture {
    _ctx: MmArc,
    coin: EthCoin,
    world: Arc<World>,
    failover: StubSender,
    payment: SignedEthTx,
    id: Vec<u8>,
    time_lock: u32,
}

fn host_of(url: &str) -> String { url.parse::<http::Uri>().unwrap().host().unwrap().to_owned() }

fn urls_of(urls: &[&str]) -> Vec<String> { urls.iter().map(|url| (*url).to_owned()).collect() }

/// The policy activation computes over `urls` with no `swap_evidence_node`.
fn listed(urls: &[&str]) -> SwapEvidencePolicy { swap_evidence_policy_from("ETH", None, &urls_of(urls)).unwrap() }

/// The policy activation computes over `urls` with `swap_evidence_node` set to `field`.
fn trusting(field: &str, urls: &[&str]) -> SwapEvidencePolicy {
    swap_evidence_policy_from("ETH", Some(field), &urls_of(urls)).unwrap()
}

/// The account nonce of the next test transaction. No test depends on a particular value: each transaction only
/// gets a nonce of its own.
static NEXT_NONCE: AtomicU64 = AtomicU64::new(1);

fn next_nonce() -> U256 { NEXT_NONCE.fetch_add(1, AtomicOrdering::Relaxed).into() }

fn sign(coin: &EthCoin, value: U256, data: Vec<u8>) -> SignedEthTx {
    coin.sign_tx_for_send(UnSignedEthTx {
        nonce: next_nonce(),
        value,
        action: Action::Call(coin.swap_contract_address),
        data,
        gas: 200_000.into(),
        gas_price: 1_000_000_000u64.into(),
    })
    .unwrap()
}

impl Fixture {
    /// An ETH coin over the nodes `urls` with `policy`. Every node starts at tip 100 with the payment sent in
    /// block 90 and known by its hash.
    fn eth(urls: &[&str], policy: SwapEvidencePolicy) -> Fixture {
        Fixture::new(EthCoinType::Eth, urls, policy, TIME_LOCK)
    }

    fn new(coin_type: EthCoinType, urls: &[&str], policy: SwapEvidencePolicy, time_lock: u32) -> Fixture {
        let world = Arc::new(World::default());
        let instances: Vec<Web3Instance> = urls
            .iter()
            .map(|url| {
                let world = world.clone();
                let (web3, _) = stub_provider(&[*url], 10., move |call| answer(&world, call));
                Web3Instance {
                    url: (*url).to_owned(),
                    web3,
                    is_parity: false,
                }
            })
            .collect();
        for url in urls {
            world.nodes.lock().unwrap().insert(host_of(url), Node {
                down: false,
                tip: 100,
                timestamp: TIP_TIME,
                states: vec![(90, SENT)],
                spend: None,
                knows_payment: true,
            });
        }
        let (web3, failover) = stub_provider(&[FAILOVER], 10., |_| {
            Reply::Fail("the failover provider must not be asked".to_owned())
        });
        let token_addr = match &coin_type {
            EthCoinType::Erc20 { token_addr, .. } => Some(*token_addr),
            _ => None,
        };
        let (ctx, mut coin) = eth_coin_for_test_with_providers(
            coin_type,
            web3,
            instances,
            None,
            mm2_net_config::SUPPORTED_NETIDS[0],
            18,
        );
        Arc::get_mut(&mut coin.0)
            .expect("the coin is not shared yet")
            .swap_evidence = policy;

        let id = coin.etomic_swap_id(time_lock, &secret_hash());
        let payment = match token_addr {
            None => {
                let data = SWAP_CONTRACT
                    .function("ethPayment")
                    .unwrap()
                    .encode_input(&[
                        Token::FixedBytes(id.clone()),
                        Token::Address(coin.my_address),
                        Token::FixedBytes(secret_hash()),
                        Token::Uint(U256::from(time_lock)),
                    ])
                    .unwrap();
                sign(&coin, paid_wei(), data)
            },
            Some(token_addr) => {
                let data = SWAP_CONTRACT
                    .function("erc20Payment")
                    .unwrap()
                    .encode_input(&[
                        Token::FixedBytes(id.clone()),
                        Token::Uint(paid_wei()),
                        Token::Address(token_addr),
                        Token::Address(coin.my_address),
                        Token::FixedBytes(secret_hash()),
                        Token::Uint(U256::from(time_lock)),
                    ])
                    .unwrap();
                sign(&coin, 0.into(), data)
            },
        };
        *world.payment.lock().unwrap() = Some(payment.clone());
        Fixture {
            _ctx: ctx,
            coin,
            world,
            failover,
            payment,
            id,
            time_lock,
        }
    }

    /// Changes the script of the node `url`.
    fn set(&self, url: &str, change: impl FnOnce(&mut Node)) {
        let mut nodes = self.world.nodes.lock().unwrap();
        change(nodes.get_mut(&host_of(url)).expect("a node of the fixture"));
    }

    /// A claim of the payment with `secret`, as a log in `block`.
    fn claim(&self, secret: &[u8; 32], block: u64) -> Spend {
        let data = SWAP_CONTRACT
            .function("receiverSpend")
            .unwrap()
            .encode_input(&[
                Token::FixedBytes(self.id.clone()),
                Token::Uint(paid_wei()),
                Token::FixedBytes(secret.to_vec()),
                Token::Address(Address::zero()),
                Token::Address(self.coin.my_address),
            ])
            .unwrap();
        Spend {
            event: "ReceiverSpent",
            block,
            tx: sign(&self.coin, 0.into(), data),
            data: [self.id.as_slice(), &secret[..]].concat(),
        }
    }

    /// A refund of the payment, as a log in `block`.
    fn refund(&self, block: u64) -> Spend {
        let data = SWAP_CONTRACT
            .function("senderRefund")
            .unwrap()
            .encode_input(&[
                Token::FixedBytes(self.id.clone()),
                Token::Uint(paid_wei()),
                Token::FixedBytes(secret_hash()),
                Token::Address(Address::zero()),
                Token::Address(self.coin.my_address),
            ])
            .unwrap();
        Spend {
            event: "SenderRefunded",
            block,
            tx: sign(&self.coin, 0.into(), data),
            data: self.id.clone(),
        }
    }

    /// The node `url` shows `spend` from the spend's block on, with the contract's state `state`.
    fn spent_on(&self, url: &str, state: u8, spend: &Spend) {
        let spend = spend.clone();
        self.set(url, move |node| {
            node.states.push((spend.block, state));
            node.spend = Some(spend);
        });
    }

    /// The methods the node `url` was asked, in arrival order.
    fn asked(&self, url: &str) -> Vec<String> {
        let host = host_of(url);
        self.world
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(seen_host, _, _)| *seen_host == host)
            .map(|(_, method, _)| method.clone())
            .collect()
    }

    /// The block of every `eth_call` the node `url` was asked.
    fn call_blocks(&self, url: &str) -> Vec<u64> {
        let host = host_of(url);
        self.world
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(seen_host, method, _)| *seen_host == host && method == "eth_call")
            .map(|(_, _, params)| block_of(&params[1]))
            .collect()
    }

    fn forget_requests(&self) { self.world.seen.lock().unwrap().clear(); }

    /// Validates the payment as its receiver; `wait_until` 0 makes one attempt.
    fn validate(&self, wait_until: u64) -> Result<(), String> {
        let key_pair = KeyPair::from_secret_slice(&hex::decode(COIN_KEY).unwrap()).unwrap();
        let sender_pub = compressed_secp256k1_public(key_pair.public());
        block_on(
            self.coin
                .validate_payment(
                    &rlp::encode(&self.payment),
                    self.time_lock,
                    &sender_pub,
                    &secret_hash(),
                    "1".parse::<BigDecimal>().unwrap(),
                    self.coin.swap_contract_address,
                    wait_until,
                )
                .compat(),
        )
    }

    fn search(&self) -> Result<Option<FoundSwapTxSpend>, String> {
        block_on(
            self.coin
                .search_for_swap_tx_spend(&rlp::encode(&self.payment), self.coin.swap_contract_address, 80),
        )
    }

    /// One poll of the spend wait: the deadline has passed when the wait begins.
    fn wait_for_spend(&self) -> Result<TransactionEnum, TransactionErr> {
        block_on(self.coin.wait_for_payment_spend(
            &self.payment,
            &self.id,
            self.coin.swap_contract_address,
            now_ms() / 1000 - 1,
            80,
            0.01,
        ))
    }

    fn refund_gate(&self, locktime: u64) -> Result<CanRefundHtlc, String> {
        block_on(self.coin.can_refund_htlc(locktime).compat())
    }

    fn assert_failover_unused(&self) {
        assert!(
            self.failover.calls().is_empty(),
            "an agreed read went through the failover provider: {:?}",
            self.failover.calls()
        );
    }
}

fn spent(spend: &Spend) -> Option<FoundSwapTxSpend> { Some(FoundSwapTxSpend::Spent(spend.tx.clone().into())) }

fn refunded(spend: &Spend) -> Option<FoundSwapTxSpend> { Some(FoundSwapTxSpend::Refunded(spend.tx.clone().into())) }

fn assert_undetermined(text: &str, reason: &str) {
    assert!(text.contains("is undetermined and the step waits"), "{text}");
    assert!(text.contains(reason), "{text}");
    assert!(
        text.contains("add a node on another host") && text.contains("swap_evidence_node"),
        "the two fixes are not named: {text}"
    );
    assert!(!text.contains("http://"), "a node is named by a URL: {text}");
}

// ---- P: the counterparty's payment state ----

#[test]
fn payment_state_two_agreeing_nodes_validate_the_payment() {
    let fx = Fixture::eth(&[A, B, C], listed(&[A, B, C]));
    fx.validate(0).unwrap();
    // Each node of the pair: its latest block, then the state at that block and the transaction.
    for node in [A, B] {
        assert_eq!(fx.asked(node), [
            "eth_getBlockByNumber",
            "eth_call",
            "eth_getTransactionByHash"
        ]);
        assert_eq!(fx.call_blocks(node), [100]);
    }
    assert!(fx.asked(C).is_empty(), "a third node was asked: {:?}", fx.asked(C));
    fx.assert_failover_unused();
}

#[test]
fn payment_state_two_differing_nodes_are_undetermined_and_a_new_agreeing_pair_decides() {
    let fx = Fixture::eth(&[A, B, C], listed(&[A, B, C]));
    // Node B's contract state does not know the payment at the block node A shows it as sent.
    fx.set(B, |node| node.states.clear());
    let err = fx.validate(0).unwrap_err();
    assert_undetermined(&err, "the nodes disagree");
    assert!(
        err.contains("a.example:8545") && err.contains("b.example:8545"),
        "{err}"
    );
    // Node C agrees with node A, and is not asked: a disagreement is not outvoted.
    assert!(fx.asked(C).is_empty(), "a third node was asked: {:?}", fx.asked(C));

    // A later attempt whose pair agrees decides.
    fx.set(B, |node| node.states = vec![(90, SENT)]);
    fx.validate(0).unwrap();
    assert!(fx.asked(C).is_empty());
    fx.assert_failover_unused();
}

#[test]
fn payment_state_one_answering_node_is_undetermined() {
    let fx = Fixture::eth(&[A, B, C], listed(&[A, B, C]));
    fx.set(B, |node| node.down = true);
    fx.set(C, |node| node.down = true);
    let err = fx.validate(0).unwrap_err();
    assert_undetermined(&err, "fewer than two nodes answered");
    fx.assert_failover_unused();
}

#[test]
fn payment_state_walks_past_a_silent_node_to_the_next_host() {
    let fx = Fixture::eth(&[A, B, C], listed(&[A, B, C]));
    fx.set(A, |node| node.down = true);
    fx.validate(0).unwrap();
    assert_eq!(fx.call_blocks(B), [100]);
    assert_eq!(fx.call_blocks(C), [100]);
}

#[test]
fn two_entries_of_one_host_are_one_node_and_no_read_is_made() {
    let policy = listed(&[A, A2]);
    assert_eq!(policy, SwapEvidencePolicy::Insufficient { hosts: 1 });
    let fx = Fixture::eth(&[A, A2], policy);
    assert!(
        fx.coin.swap_evidence_refusal().is_some(),
        "admission does not refuse the coin"
    );

    let err = fx.validate(0).unwrap_err();
    assert_undetermined(&err, "fewer than two nodes answered");
    let gate = fx.refund_gate(now_ms() / 1000 - 7200);
    assert!(
        matches!(gate, Err(e) if is_undetermined(&e)),
        "an undetermined gate is not open and not a plain wait"
    );
    assert!(
        fx.world.seen.lock().unwrap().is_empty(),
        "a node was asked for a read nothing can decide"
    );
    fx.assert_failover_unused();
}

#[test]
fn payment_state_is_read_at_each_nodes_own_tip() {
    let fx = Fixture::eth(&[A, B], listed(&[A, B]));
    // Node B is one block and twelve seconds behind; each shows the payment as sent at its own tip.
    fx.set(A, |node| node.tip = 101);
    fx.set(B, |node| node.timestamp = TIP_TIME - 12);
    fx.validate(0).unwrap();
    assert_eq!(fx.call_blocks(A), [101]);
    assert_eq!(fx.call_blocks(B), [100]);

    // The payment is only in node A's newest block: node B does not show it yet, so the two disagree and the
    // payment is not validated on node A alone. Once node B has the block, it is.
    fx.set(A, |node| node.states = vec![(101, SENT)]);
    fx.set(B, |node| node.states = vec![(101, SENT)]);
    assert_undetermined(&fx.validate(0).unwrap_err(), "the nodes disagree");
    fx.set(B, |node| node.tip = 101);
    fx.validate(0).unwrap();
}

#[test]
fn a_tip_gap_above_the_bound_is_undetermined() {
    let fx = Fixture::eth(&[A, B], listed(&[A, B]));
    // Node B's latest block is 61 seconds older than node A's; both show the same states.
    fx.set(B, |node| node.timestamp = TIP_TIME - 61);
    assert_undetermined(&fx.validate(0).unwrap_err(), "the tips of the nodes differ too much");
    // Never "not spent", so nothing is ready for a refund.
    assert_undetermined(&fx.search().unwrap_err(), "the tips of the nodes differ too much");
    // A refund both nodes show is not taken across the gap either.
    let refund = fx.refund(95);
    fx.spent_on(A, REFUNDED, &refund);
    fx.spent_on(B, REFUNDED, &refund);
    assert_undetermined(&fx.search().unwrap_err(), "the tips of the nodes differ too much");

    // At the bound itself the two answers are compared.
    fx.set(B, |node| node.timestamp = TIP_TIME - 60);
    let err = fx.validate(0).unwrap_err();
    assert!(err.contains("Payment state is not PAYMENT_STATE_SENT, got 3"), "{err}");
    assert_eq!(fx.search(), Ok(refunded(&refund)));
    fx.assert_failover_unused();
}

#[test]
fn payment_state_of_the_trusted_node_is_taken_alone() {
    let fx = Fixture::eth(&[A, B], trusting("b.example:8545", &[A, B]));
    // Node A does not know the payment; it is not asked.
    fx.set(A, |node| node.states.clear());
    fx.validate(0).unwrap();
    assert!(fx.asked(A).is_empty(), "{:?}", fx.asked(A));
    assert_eq!(fx.asked(B), [
        "eth_getBlockByNumber",
        "eth_call",
        "eth_getTransactionByHash"
    ]);
}

#[test]
fn payment_state_with_a_silent_trusted_node_is_undetermined() {
    let fx = Fixture::eth(&[A, B], trusting("b.example:8545", &[A, B]));
    fx.set(B, |node| node.down = true);
    let err = fx.validate(0).unwrap_err();
    assert_undetermined(&err, "the trusted node did not answer");
    // No other node is asked in its place.
    assert!(fx.asked(A).is_empty(), "{:?}", fx.asked(A));
}

#[test]
fn payment_state_of_a_token_is_agreed_over_the_same_nodes() {
    let coin_type = EthCoinType::Erc20 {
        platform: "ETH".to_string(),
        token_addr: Address::from_slice(&hex::decode("c0eb7aed740e1796992a08962c15661bdeb58003").unwrap()),
    };
    let fx = Fixture::new(coin_type, &[A, B], listed(&[A, B]), TIME_LOCK);
    fx.validate(0).unwrap();
    assert_eq!(fx.call_blocks(A), [100]);
    assert_eq!(fx.call_blocks(B), [100]);
    fx.assert_failover_unused();

    fx.set(B, |node| node.states.clear());
    assert_undetermined(&fx.validate(0).unwrap_err(), "the nodes disagree");
}

// ---- S: claimed, refunded or not spent ----

#[test]
fn spend_state_two_agreeing_nodes_decide_each_outcome() {
    let fx = Fixture::eth(&[A, B, C], listed(&[A, B, C]));
    assert_eq!(fx.search(), Ok(None));
    assert!(fx.asked(C).is_empty(), "a third node was asked: {:?}", fx.asked(C));

    // A claim whose secret is not the swap's does not prove itself: it is taken on the agreement of the pair.
    let claim = fx.claim(&OTHER_SECRET, 95);
    fx.spent_on(A, SPENT, &claim);
    fx.spent_on(B, SPENT, &claim);
    assert_eq!(fx.search(), Ok(spent(&claim)));

    let fx = Fixture::eth(&[A, B, C], listed(&[A, B, C]));
    let refund = fx.refund(95);
    fx.spent_on(A, REFUNDED, &refund);
    fx.spent_on(B, REFUNDED, &refund);
    assert_eq!(fx.search(), Ok(refunded(&refund)));
    fx.assert_failover_unused();
}

#[test]
fn spend_state_two_differing_nodes_are_undetermined_and_a_new_agreeing_pair_decides() {
    let fx = Fixture::eth(&[A, B, C], listed(&[A, B, C]));
    let refund = fx.refund(95);
    fx.spent_on(A, REFUNDED, &refund);
    fx.spent_on(C, REFUNDED, &refund);
    // Node B shows the payment as not spent at the same block.
    let err = fx.search().unwrap_err();
    assert_undetermined(&err, "the nodes disagree");
    assert!(
        err.contains("a.example:8545") && err.contains("b.example:8545"),
        "{err}"
    );
    assert!(fx.asked(C).is_empty(), "a third node was asked: {:?}", fx.asked(C));

    fx.spent_on(B, REFUNDED, &refund);
    assert_eq!(fx.search(), Ok(refunded(&refund)));
}

#[test]
fn spend_state_one_answering_node_never_reads_as_not_spent() {
    let fx = Fixture::eth(&[A, B, C], listed(&[A, B, C]));
    fx.set(B, |node| node.down = true);
    fx.set(C, |node| node.down = true);
    assert_undetermined(&fx.search().unwrap_err(), "fewer than two nodes answered");

    // The same with a claim that does not carry the swap's secret.
    let claim = fx.claim(&OTHER_SECRET, 95);
    fx.spent_on(A, SPENT, &claim);
    assert_undetermined(&fx.search().unwrap_err(), "fewer than two nodes answered");
}

#[test]
fn spend_state_of_two_nodes_within_the_gap_that_agree_is_decided() {
    let fx = Fixture::eth(&[A, B], listed(&[A, B]));
    // Node B is one block and twelve seconds behind.
    fx.set(A, |node| node.tip = 101);
    fx.set(B, |node| node.timestamp = TIP_TIME - 12);
    assert_eq!(fx.search(), Ok(None));
    assert_eq!(fx.call_blocks(A), [101]);
    assert_eq!(fx.call_blocks(B), [100]);

    // Both show the same claim, which does not carry the swap's secret.
    let claim = fx.claim(&OTHER_SECRET, 95);
    fx.spent_on(A, SPENT, &claim);
    fx.spent_on(B, SPENT, &claim);
    assert_eq!(fx.search(), Ok(spent(&claim)));
}

#[test]
fn a_claim_with_the_matching_secret_at_the_newer_tip_is_taken() {
    // The claim is only in node A's newest block; node B, one block behind, still shows "not spent".
    let fx = Fixture::eth(&[A, B], listed(&[A, B]));
    let claim = fx.claim(&SECRET, 101);
    fx.set(A, |node| node.tip = 101);
    fx.spent_on(A, SPENT, &claim);
    fx.set(B, |node| node.timestamp = TIP_TIME - 12);
    assert_eq!(fx.search(), Ok(spent(&claim)));
    assert_eq!(fx.wait_for_spend().unwrap(), TransactionEnum::from(claim.tx.clone()));

    // The same with node B beyond the gap: a lagging node does not hide the secret.
    fx.set(B, |node| node.timestamp = TIP_TIME - 600);
    assert_eq!(fx.search(), Ok(spent(&claim)));
    assert_eq!(fx.wait_for_spend().unwrap(), TransactionEnum::from(claim.tx.clone()));
}

#[test]
fn a_claim_with_another_secret_at_the_newer_tip_is_not_taken() {
    let fx = Fixture::eth(&[A, B], listed(&[A, B]));
    let claim = fx.claim(&OTHER_SECRET, 101);
    fx.set(A, |node| node.tip = 101);
    fx.spent_on(A, SPENT, &claim);
    fx.set(B, |node| node.timestamp = TIP_TIME - 12);
    // Node B does not show the claim yet: no claim, and no "not spent" either.
    assert_undetermined(&fx.search().unwrap_err(), "the nodes disagree");
    assert!(fx.wait_for_spend().is_err());

    // Once node B shows it too, the pair decides.
    fx.set(B, |node| node.tip = 101);
    fx.spent_on(B, SPENT, &claim);
    assert_eq!(fx.search(), Ok(spent(&claim)));
}

#[test]
fn a_claim_with_the_matching_secret_is_taken_on_one_node() {
    // The other nodes are silent.
    let fx = Fixture::eth(&[A, B, C], listed(&[A, B, C]));
    let claim = fx.claim(&SECRET, 95);
    fx.spent_on(A, SPENT, &claim);
    fx.set(B, |node| node.down = true);
    fx.set(C, |node| node.down = true);
    assert_eq!(fx.search(), Ok(spent(&claim)));

    // The other node of the pair shows the payment as not spent: the secret still proves the claim.
    let fx = Fixture::eth(&[A, B], listed(&[A, B]));
    let claim = fx.claim(&SECRET, 95);
    fx.spent_on(A, SPENT, &claim);
    assert_eq!(fx.search(), Ok(spent(&claim)));
    fx.assert_failover_unused();
}

#[test]
fn a_claim_with_another_secret_is_not_taken_on_one_node() {
    let fx = Fixture::eth(&[A, B], listed(&[A, B]));
    let claim = fx.claim(&OTHER_SECRET, 95);
    fx.spent_on(A, SPENT, &claim);
    // Node B shows the payment as not spent.
    assert_undetermined(&fx.search().unwrap_err(), "the nodes disagree");
}

#[test]
fn spend_state_of_the_trusted_node_is_taken_alone() {
    let fx = Fixture::eth(&[A, B], trusting("b.example:8545", &[A, B]));
    let refund = fx.refund(95);
    fx.spent_on(A, REFUNDED, &refund);
    // The trusted node B shows the payment as not spent; node A is not asked.
    assert_eq!(fx.search(), Ok(None));
    assert!(fx.asked(A).is_empty(), "{:?}", fx.asked(A));
}

#[test]
fn spend_state_with_a_silent_trusted_node_is_undetermined_unless_a_secret_proves_the_claim() {
    let fx = Fixture::eth(&[A, B], trusting("b.example:8545", &[A, B]));
    fx.set(B, |node| node.down = true);
    // Node A's "not spent" is not taken in the trusted node's place.
    assert_undetermined(&fx.search().unwrap_err(), "the trusted node did not answer");

    // A claim with the swap's secret proves itself on any node.
    let claim = fx.claim(&SECRET, 95);
    fx.spent_on(A, SPENT, &claim);
    assert_eq!(fx.search(), Ok(spent(&claim)));
}

#[test]
fn spend_state_of_a_coin_with_one_host_takes_only_a_claim_that_proves_itself() {
    let fx = Fixture::eth(&[A, A2], listed(&[A, A2]));
    assert_undetermined(&fx.search().unwrap_err(), "fewer than two nodes answered");

    let other = fx.claim(&OTHER_SECRET, 95);
    fx.spent_on(A, SPENT, &other);
    assert_undetermined(&fx.search().unwrap_err(), "fewer than two nodes answered");

    let fx = Fixture::eth(&[A, A2], listed(&[A, A2]));
    let claim = fx.claim(&SECRET, 95);
    fx.spent_on(A, SPENT, &claim);
    assert_eq!(fx.search(), Ok(spent(&claim)));
}

#[test]
fn the_spend_wait_returns_a_claim_and_runs_to_its_deadline_otherwise() {
    // Not spent on both nodes: the wait ends at its deadline.
    let fx = Fixture::eth(&[A, B], listed(&[A, B]));
    let err = fx.wait_for_spend().unwrap_err();
    assert!(err.get_plain_text_format().contains("Waited too long"), "{err:?}");

    // Undetermined (one node answers "not spent"): the wait ends at its deadline too, never with a spend.
    fx.set(B, |node| node.down = true);
    let err = fx.wait_for_spend().unwrap_err();
    assert!(err.get_plain_text_format().contains("Waited too long"), "{err:?}");

    // A claim with the swap's secret on the one answering node is returned.
    let claim = fx.claim(&SECRET, 95);
    fx.spent_on(A, SPENT, &claim);
    let found = fx.wait_for_spend().unwrap();
    assert_eq!(found, TransactionEnum::from(claim.tx.clone()));

    // A claim with another secret on the one answering node is not.
    let fx = Fixture::eth(&[A, B], listed(&[A, B]));
    let claim = fx.claim(&OTHER_SECRET, 95);
    fx.spent_on(A, SPENT, &claim);
    fx.set(B, |node| node.down = true);
    assert!(fx.wait_for_spend().is_err());
    // Both nodes showing it decide.
    fx.set(B, |node| node.down = false);
    fx.spent_on(B, SPENT, &claim);
    assert_eq!(fx.wait_for_spend().unwrap(), TransactionEnum::from(claim.tx.clone()));
    fx.assert_failover_unused();
}

// ---- T: the chain time of the refund gate ----

#[test]
fn refund_gate_opens_on_two_different_times_past_the_lock() {
    let fx = Fixture::eth(&[A, B, C], listed(&[A, B, C]));
    let locktime = now_ms() / 1000 - 7200;
    fx.set(A, |node| node.timestamp = locktime + 1);
    fx.set(B, |node| node.timestamp = locktime + 900);
    let gate = fx.refund_gate(locktime);
    assert!(matches!(gate, Ok(CanRefundHtlc::CanRefundNow)));
    assert_eq!(fx.asked(A), ["eth_getBlockByNumber"]);
    assert_eq!(fx.asked(B), ["eth_getBlockByNumber"]);
    assert!(fx.asked(C).is_empty(), "a third node was asked: {:?}", fx.asked(C));
    fx.assert_failover_unused();
}

#[test]
fn refund_gate_stays_closed_when_one_time_is_not_past_the_lock() {
    let fx = Fixture::eth(&[A, B, C], listed(&[A, B, C]));
    let locktime = now_ms() / 1000 - 7200;
    // Node C is past the lock too; it is not asked to outvote node B.
    fx.set(A, |node| node.timestamp = locktime + 100);
    fx.set(B, |node| node.timestamp = locktime - 100);
    fx.set(C, |node| node.timestamp = locktime + 100);
    let gate = fx.refund_gate(locktime);
    assert!(
        matches!(gate, Err(e) if is_undetermined(&e)),
        "an undetermined gate is not open and not a plain wait"
    );
    assert!(fx.asked(C).is_empty(), "a third node was asked: {:?}", fx.asked(C));

    // Both nodes at the lock time exactly: they agree, and the gate is closed.
    fx.set(A, |node| node.timestamp = locktime);
    fx.set(B, |node| node.timestamp = locktime);
    let gate = fx.refund_gate(locktime);
    assert!(matches!(gate, Ok(CanRefundHtlc::HaveToWait(_))));

    // A later attempt whose pair is past the lock opens it.
    fx.set(A, |node| node.timestamp = locktime + 5);
    fx.set(B, |node| node.timestamp = locktime + 7);
    let gate = fx.refund_gate(locktime);
    assert!(matches!(gate, Ok(CanRefundHtlc::CanRefundNow)));
}

#[test]
fn refund_gate_stays_closed_on_one_answering_node() {
    let fx = Fixture::eth(&[A, B], listed(&[A, B]));
    let locktime = now_ms() / 1000 - 7200;
    fx.set(A, |node| node.timestamp = locktime + 100);
    fx.set(B, |node| node.down = true);
    let gate = fx.refund_gate(locktime);
    assert!(
        matches!(gate, Err(e) if is_undetermined(&e)),
        "an undetermined gate is not open and not a plain wait"
    );
}

#[test]
fn refund_gate_follows_the_trusted_node_alone() {
    let fx = Fixture::eth(&[A, B], trusting("b.example:8545", &[A, B]));
    let locktime = now_ms() / 1000 - 7200;
    fx.set(A, |node| node.timestamp = locktime - 100);
    fx.set(B, |node| node.timestamp = locktime + 100);
    let gate = fx.refund_gate(locktime);
    assert!(matches!(gate, Ok(CanRefundHtlc::CanRefundNow)));
    assert!(fx.asked(A).is_empty(), "{:?}", fx.asked(A));

    // The trusted node is silent: the gate stays closed and no other node is asked.
    fx.forget_requests();
    fx.set(A, |node| node.timestamp = locktime + 100);
    fx.set(B, |node| node.down = true);
    let gate = fx.refund_gate(locktime);
    assert!(
        matches!(gate, Err(e) if is_undetermined(&e)),
        "an undetermined gate is not open and not a plain wait"
    );
    assert!(fx.asked(A).is_empty(), "{:?}", fx.asked(A));
}

#[test]
fn refund_gate_before_the_lock_time_asks_no_node() {
    let fx = Fixture::eth(&[A, B], listed(&[A, B]));
    let locktime = now_ms() / 1000 + 1000;
    let gate = block_on(fx.coin.can_refund_htlc(locktime).compat());
    assert!(matches!(gate, Ok(CanRefundHtlc::HaveToWait(wait)) if (1000..=1002).contains(&wait)));
    assert!(fx.world.seen.lock().unwrap().is_empty());
}

// ---- The error form the swap engine recognizes ----

#[test]
fn every_undetermined_read_is_recognized_by_the_core_and_a_decided_error_is_not() {
    // Two nodes that disagree: node A shows the payment as refunded, node B as sent.
    let fx = Fixture::eth(&[A, B], listed(&[A, B]));
    let refund = fx.refund(95);
    fx.spent_on(A, REFUNDED, &refund);
    let locktime = now_ms() / 1000 - 7200;
    fx.set(A, |node| node.timestamp = locktime + 30);
    fx.set(B, |node| node.timestamp = locktime - 30);
    assert!(is_undetermined(&fx.validate(0).unwrap_err()), "the payment check");
    assert!(is_undetermined(&fx.search().unwrap_err()), "the spend search");
    assert!(
        is_undetermined(&fx.wait_for_spend().unwrap_err().get_plain_text_format()),
        "the spend wait that ends on an undetermined read"
    );
    assert!(
        matches!(fx.refund_gate(locktime), Err(e) if is_undetermined(&e)),
        "the refund gate"
    );

    // A tip gap.
    let fx = Fixture::eth(&[A, B], listed(&[A, B]));
    fx.set(B, |node| node.timestamp = TIP_TIME - 61);
    assert!(
        is_undetermined(&fx.validate(0).unwrap_err()),
        "the payment check across a tip gap"
    );
    assert!(
        is_undetermined(&fx.search().unwrap_err()),
        "the spend search across a tip gap"
    );

    // Too few nodes, and a silent trusted node.
    let fx = Fixture::eth(&[A, A2], listed(&[A, A2]));
    assert!(is_undetermined(&fx.validate(0).unwrap_err()), "one host");
    let fx = Fixture::eth(&[A, B], trusting("b.example:8545", &[A, B]));
    fx.set(B, |node| node.down = true);
    assert!(is_undetermined(&fx.search().unwrap_err()), "a silent trusted node");

    // Decided answers that are errors: a payment both nodes show as refunded, and a spend wait that ends with
    // both nodes agreeing on "not spent".
    let fx = Fixture::eth(&[A, B], listed(&[A, B]));
    let err = fx.wait_for_spend().unwrap_err().get_plain_text_format();
    assert!(err.contains("Waited too long") && !is_undetermined(&err), "{err}");
    let refund = fx.refund(95);
    fx.spent_on(A, REFUNDED, &refund);
    fx.spent_on(B, REFUNDED, &refund);
    let err = fx.validate(0).unwrap_err();
    assert!(err.contains("Payment state is not PAYMENT_STATE_SENT, got 3"), "{err}");
    assert!(!is_undetermined(&err), "{err}");
}

// ---- Admission and logs ----

#[test]
fn the_evm_family_answers_its_stored_policy() {
    let two = listed(&[A, B]);
    let fx = Fixture::eth(&[A, B], two.clone());
    assert_eq!(fx.coin.swap_evidence_policy(), two);
    assert_eq!(fx.coin.swap_evidence_refusal(), None);

    // One probed node and no `swap_evidence_node`: an order and a swap start are refused with this text.
    let fx = Fixture::eth(&[A], listed(&[A]));
    assert_eq!(
        fx.coin.swap_evidence_refusal().as_deref(),
        Some("ETH has fewer than two nodes for swap evidence; add a second node or set swap_evidence_node")
    );
    // With the field naming that node the coin may trade.
    let fx = Fixture::eth(&[A], trusting("a.example", &[A]));
    assert_eq!(fx.coin.swap_evidence_refusal(), None);
}

#[test]
fn a_trusted_node_without_a_port_in_its_address_is_matched() {
    // The address writes no port, so the node is named by its host and the default port of its scheme.
    const PLAIN: &str = "https://plain.example/v3/KEY";
    let fx = Fixture::eth(&[A, PLAIN], trusting("plain.example", &[A, PLAIN]));
    fx.set(A, |node| node.states.clear());
    fx.validate(0).unwrap();
    assert!(fx.asked(A).is_empty(), "{:?}", fx.asked(A));
    assert_eq!(fx.call_blocks(PLAIN), [100]);

    fx.set(PLAIN, |node| node.down = true);
    let err = fx.validate(0).unwrap_err();
    assert_undetermined(&err, "the trusted node did not answer");
    assert!(err.contains("plain.example:443"), "{err}");
    assert!(!err.contains("KEY") && !err.contains("https://"), "{err}");
}

#[test]
fn an_undetermined_read_is_warned_about_once_without_a_node_and_logged_in_full_at_debug() {
    let capturing = install_log_capture();
    // A lock time of its own gives this test a payment id no other test reports.
    let fx = Fixture::new(EthCoinType::Eth, &[A, B], listed(&[A, B]), TIME_LOCK + 77);
    fx.set(B, |node| node.states.clear());
    let first = fx.validate(0).unwrap_err();
    let second = fx.validate(0).unwrap_err();
    assert_eq!(first, second);
    assert_undetermined(&first, "the nodes disagree");
    assert!(
        first.contains("a.example:8545: payment state 1") && first.contains("b.example:8545: payment state 0"),
        "{first}"
    );
    assert!(
        capturing,
        "the log capture is not installed (another logger owns the process), so the WARN-once rule is not checked"
    );
    let id = hex::encode(&fx.id);
    let lines: Vec<String> = captured_log_lines()
        .into_iter()
        .filter(|line| line.contains(&id) && line.contains("is undetermined"))
        .collect();
    let warnings: Vec<&String> = lines.iter().filter(|line| line.starts_with("WARN")).collect();
    assert_eq!(warnings.len(), 1, "{lines:?}");
    // The warning names the kind and the fixes, and no node: no host, no label, no answer of a node.
    let warning = warnings[0];
    assert!(
        warning.contains("the nodes disagree") && warning.contains("swap_evidence_node"),
        "{warning}"
    );
    for node_text in ["a.example", "b.example", "8545", "payment state 1", "payment state 0"] {
        assert!(
            !warning.contains(node_text),
            "the warning carries `{node_text}`: {warning}"
        );
    }
    // What each node said is in the DEBUG lines, one per undetermined read.
    let debug_lines: Vec<&String> = lines.iter().filter(|line| line.starts_with("DEBUG")).collect();
    assert_eq!(debug_lines.len(), 2, "{lines:?}");
    for line in debug_lines {
        assert!(
            line.contains("a.example:8545: payment state 1") && line.contains("b.example:8545: payment state 0"),
            "{line}"
        );
    }
}

// ---- The readiness check before a swap starts (maintainer decision of 2026-11-25/26 under R100) ----

/// How many readiness probes (`eth_blockNumber`) each host got so far.
fn probes_of(fx: &Fixture, host: &str) -> usize {
    fx.world
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(seen, method, _)| seen == host && method == "eth_blockNumber")
        .count()
}

fn set_down(fx: &Fixture, url: &str, down: bool) {
    fx.world.nodes.lock().unwrap().get_mut(&host_of(url)).unwrap().down = down;
}

/// Two of three hosts answering: ready; one of three: not ready, with a text that names no host. Unknown hosts are
/// probed once each, and not again within the window.
#[test]
fn readiness_needs_two_answering_hosts_and_probes_once_per_window() {
    const RA: &str = "http://rdy-a.example:8545";
    const RB: &str = "http://rdy-b.example:8545";
    const RC: &str = "http://rdy-c.example:8545";
    let fx = Fixture::eth(&[RA, RB, RC], listed(&[RA, RB, RC]));
    set_down(&fx, RC, true);
    assert_eq!(block_on(fx.coin.swap_evidence_ready()), Ok(()));
    assert_eq!(probes_of(&fx, "rdy-a.example"), 1);
    // The answers of the probes count for the window: nothing extra is sent.
    assert_eq!(block_on(fx.coin.swap_evidence_ready()), Ok(()));
    assert_eq!(
        probes_of(&fx, "rdy-a.example"),
        1,
        "the healthy case sends nothing extra"
    );
    assert_eq!(
        probes_of(&fx, "rdy-c.example"),
        1,
        "a failed host is not probed again within the window"
    );

    const SA: &str = "http://rdy-sa.example:8545";
    const SB: &str = "http://rdy-sb.example:8545";
    const SC: &str = "http://rdy-sc.example:8545";
    let fx = Fixture::eth(&[SA, SB, SC], listed(&[SA, SB, SC]));
    set_down(&fx, SB, true);
    set_down(&fx, SC, true);
    let error = block_on(fx.coin.swap_evidence_ready()).unwrap_err();
    assert_eq!(
        error,
        "ETH: swap evidence not available (fewer than two hosts answering)"
    );
    assert!(!error.contains("example"), "{error}");
    set_down(&fx, SB, false);
    assert!(
        block_on(fx.coin.swap_evidence_ready()).is_err(),
        "no new probe within the window"
    );
    assert_eq!(probes_of(&fx, "rdy-sb.example"), 1);
}

/// A trusted node: ready while it answers, not while it does not, whatever the other node does.
#[test]
fn readiness_of_a_trusted_node() {
    const TA: &str = "http://rdy-ta.example:8545";
    const TB: &str = "http://rdy-tb.example:8545";
    let fx = Fixture::eth(&[TA, TB], trusting(TB, &[TA, TB]));
    set_down(&fx, TB, true);
    assert_eq!(
        block_on(fx.coin.swap_evidence_ready()),
        Err("ETH: swap evidence not available (the trusted node is not answering)".to_owned())
    );
    assert_eq!(
        probes_of(&fx, "rdy-ta.example"),
        0,
        "another node never stands in for the trusted one"
    );

    const UA: &str = "http://rdy-ua.example:8545";
    const UB: &str = "http://rdy-ub.example:8545";
    let fx = Fixture::eth(&[UA, UB], trusting(UB, &[UA, UB]));
    set_down(&fx, UA, true);
    assert_eq!(block_on(fx.coin.swap_evidence_ready()), Ok(()));
}

/// The readiness probe bounds its own exchange by the probe bound of 3 s, not by the 30 s of the node's direct
/// provider, so a node that accepts and never answers keeps nothing running past the probe.
#[test]
fn the_readiness_probe_bounds_its_own_exchange() {
    const PA: &str = "http://rdy-pa.example:8545";
    let (web3, sender) = stub_provider(&[PA], 30., |call| {
        Reply::Body(result_body(&call.request, &json!("0x64").to_string()))
    });
    let instance = Web3Instance {
        url: PA.to_owned(),
        web3,
        is_parity: false,
    };
    assert!(block_on(instance.readiness_probe()));
    assert_eq!(sender.bounds(), vec![crate::swap_evidence::READY_PROBE_TIMEOUT_SECS]);
}
