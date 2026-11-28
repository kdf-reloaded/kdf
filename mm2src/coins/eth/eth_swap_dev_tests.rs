//! Local `geth --dev` integration tests for EVM: the v1 ETH/ERC20 HTLC swap
//! payment + refund path (`send_maker_payment` -> `send_maker_refunds_payment`)
//! and plain ETH/ERC20 `withdraw`.
//!
//! They deploy a clean-room `EtomicSwap` HTLC contract and a minimal ERC20 to a
//! throwaway `geth --dev` chain (the shared `mm2_test_helpers::geth_dev` harness),
//! broadcast the real transactions, and assert the on-chain outcome. They skip
//! themselves when `geth` is not on `PATH`, so the default offline suite stays
//! green; a CI job with geth installed runs them.

use super::*;
use common::{block_on, now_ms};
use mm2_core::mm_ctx::{MmArc, MmCtxBuilder};
use mm2_test_helpers::geth_dev::{GethDev, ETOMIC_SWAP_BYTECODE, TEST_ERC20_BYTECODE};
use serde_json::json;

/// Test-only DEX-fee destination pubkey (community netid), used as the swap receiver.
fn test_dex_fee_addr_raw_pubkey() -> &'static [u8] {
    mm2_net_config::net_config_or_panic(8762).dex_fee_addr_raw_pubkey()
}

/// Well-known throwaway test key (the same one the ignored upstream tests used).
const TEST_PRIV_KEY: &str = "809465b17d0a4ddb3e4c69e8f23c2cabad868f51f8bed5c765ad1d6516c3306f";

/// 100 ETH in wei — plenty of gas + value for the throwaway test key.
const HUNDRED_ETH_WEI: u128 = 100_000_000_000_000_000_000;

fn test_key_pair() -> KeyPair { KeyPair::from_secret_slice(&hex::decode(TEST_PRIV_KEY).unwrap()).unwrap() }

/// `0x`-prefixed lowercase hex of an [`Address`] (H160), as the JSON-RPC harness expects.
fn addr_hex(addr: Address) -> String { format!("0x{:x}", addr) }

/// Builds an `EthCoin` bound to the local geth node, using the throwaway test key.
/// Returns the `MmArc` too: the coin only holds a weak ctx ref, so the caller must
/// keep the returned `MmArc` alive for the lifetime of the coin.
fn dev_eth_coin(coin_type: EthCoinType, node: &GethDev, swap_contract: Address) -> (MmArc, EthCoin) {
    let key_pair = test_key_pair();
    let my_addr = key_pair.address();
    let web3 = crate::eth::alloy_compat::build_provider(
        crate::node_url::listed_nodes("TEST", vec![node.rpc_url.clone()]),
        vec![],
    )
    .unwrap();
    let ctx = MmCtxBuilder::new().into_mm_arc();
    let ticker = match &coin_type {
        EthCoinType::Erc20 { .. } => "TEST".to_string(),
        _ => "ETH".to_string(),
    };
    let coin = EthCoin(Arc::new(EthCoinImpl {
        swap_evidence: crate::SwapEvidencePolicy::NotAdopted,
        ticker,
        coin_type,
        my_address: my_addr,
        sign_message_prefix: Some(String::from("Ethereum Signed Message:\n")),
        signer: EthSigner::Local(key_pair),
        swap_contract_address: swap_contract,
        fallback_swap_contract: None,
        web3_instances: vec![Web3Instance {
            url: String::new(),
            web3: web3.clone(),
            is_parity: false,
        }],
        web3,
        decimals: 18,
        gas_station_url: None,
        gas_station_decimals: ETH_GAS_STATION_DECIMALS,
        gas_station_policy: GasStationPricePolicy::MeanAverageFast,
        history_sync_state: Mutex::new(HistorySyncState::NotStarted),
        ctx: ctx.weak(),
        required_confirmations: 1.into(),
        tron_api: None,
        tron_gasfree: None,
        nft_swap_v2_contract: None,
        swap_gas_fee_policy: Mutex::new(SwapGasFeePolicy::default()),
        erc20_tokens_infos: Default::default(),
        custom_token: false,
        chain_id: Some(node.chain_id),
        logs_block_range: DEFAULT_LOGS_BLOCK_RANGE,
        logs_window: Default::default(),
        derivation_method: DerivationMethod::Iguana(my_addr),
        swap_v2_contracts: None,
        gas_limit: EthGasLimit::default(),
        gas_limit_v2: EthGasLimitV2::default(),
        estimate_gas_mult: None,
        taker_fee_quote_at_limit: true,
    }));
    (ctx, coin)
}

/// Deploys the ERC20 fixture (dev account gets the whole supply) and moves
/// `amount` (base units) to `to`. Returns the token address.
fn deploy_erc20_and_fund(node: &GethDev, to: Address, amount: U256) -> Address {
    let token_addr: Address = node.deploy(TEST_ERC20_BYTECODE, "").parse().unwrap();
    let transfer_data = ERC20_CONTRACT
        .function("transfer")
        .unwrap()
        .encode_input(&[Token::Address(to), Token::Uint(amount)])
        .unwrap();
    node.send_call(&addr_hex(token_addr), &transfer_data);
    token_addr
}

#[test]
fn send_and_refund_eth_payment() {
    let node = match GethDev::start() {
        Some(node) => node,
        None => {
            log!("geth binary not available; skipping send_and_refund_eth_payment");
            return;
        },
    };

    let test_addr = test_key_pair().address();
    node.fund_eth(&addr_hex(test_addr), HUNDRED_ETH_WEI);

    let swap_addr: Address = node.deploy(ETOMIC_SWAP_BYTECODE, "").parse().unwrap();
    let (_ctx, coin) = dev_eth_coin(EthCoinType::Eth, &node, swap_addr);

    let secret_hash = [1u8; 20];
    // A timelock in the past so the refund is immediately valid.
    let time_lock = (now_ms() / 1000) as u32 - 200;

    let payment = coin
        .send_maker_payment(
            time_lock,
            &[],
            test_dex_fee_addr_raw_pubkey(),
            &secret_hash,
            "0.001".parse().unwrap(),
            &coin.swap_contract_address(),
        )
        .wait()
        .unwrap();

    let payment_hash = format!("0x{}", hex::encode(payment.tx_hash().0));
    let receipt = node.wait_receipt(&payment_hash);
    assert_eq!(receipt["status"].as_str(), Some("0x1"), "payment tx reverted");

    let refund = coin
        .send_maker_refunds_payment(
            &payment.tx_hex(),
            time_lock,
            test_dex_fee_addr_raw_pubkey(),
            &secret_hash,
            &[],
            &coin.swap_contract_address(),
        )
        .wait()
        .unwrap();

    let refund_hash = format!("0x{}", hex::encode(refund.tx_hash().0));
    let refund_receipt = node.wait_receipt(&refund_hash);
    assert_eq!(refund_receipt["status"].as_str(), Some("0x1"), "refund tx reverted");

    // The payment must now be in the SenderRefunded state (3).
    let id = coin.etomic_swap_id(time_lock, &secret_hash);
    let state = coin.payment_status(swap_addr, Token::FixedBytes(id)).wait().unwrap();
    assert_eq!(state, U256::from(3u64), "payment state should be SenderRefunded");
}

#[test]
fn send_and_refund_erc20_payment() {
    let node = match GethDev::start() {
        Some(node) => node,
        None => {
            log!("geth binary not available; skipping send_and_refund_erc20_payment");
            return;
        },
    };

    let test_addr = test_key_pair().address();
    node.fund_eth(&addr_hex(test_addr), HUNDRED_ETH_WEI);

    let swap_addr: Address = node.deploy(ETOMIC_SWAP_BYTECODE, "").parse().unwrap();
    let token_addr = deploy_erc20_and_fund(&node, test_addr, U256::from(1000u64) * U256::exp10(18));

    let (_ctx, coin) = dev_eth_coin(
        EthCoinType::Erc20 {
            platform: "ETH".to_string(),
            token_addr,
        },
        &node,
        swap_addr,
    );

    let secret_hash = [1u8; 20];
    let time_lock = (now_ms() / 1000) as u32 - 200;

    let payment = coin
        .send_maker_payment(
            time_lock,
            &[],
            test_dex_fee_addr_raw_pubkey(),
            &secret_hash,
            "0.001".parse().unwrap(),
            &coin.swap_contract_address(),
        )
        .wait()
        .unwrap();

    let payment_hash = format!("0x{}", hex::encode(payment.tx_hash().0));
    let receipt = node.wait_receipt(&payment_hash);
    assert_eq!(receipt["status"].as_str(), Some("0x1"), "erc20 payment tx reverted");

    let refund = coin
        .send_maker_refunds_payment(
            &payment.tx_hex(),
            time_lock,
            test_dex_fee_addr_raw_pubkey(),
            &secret_hash,
            &[],
            &coin.swap_contract_address(),
        )
        .wait()
        .unwrap();

    let refund_hash = format!("0x{}", hex::encode(refund.tx_hash().0));
    let refund_receipt = node.wait_receipt(&refund_hash);
    assert_eq!(
        refund_receipt["status"].as_str(),
        Some("0x1"),
        "erc20 refund tx reverted"
    );

    let id = coin.etomic_swap_id(time_lock, &secret_hash);
    let state = coin.payment_status(swap_addr, Token::FixedBytes(id)).wait().unwrap();
    assert_eq!(state, U256::from(3u64), "payment state should be SenderRefunded");
}

/// The coin of [`dev_eth_coin`] with its swap reads agreed over the nodes `urls` (CRD ch.51 R100): one directed
/// provider per URL, and the policy activation computes for that list.
fn dev_eth_coin_agreeing_over(
    coin_type: EthCoinType,
    node: &GethDev,
    swap_contract: Address,
    urls: &[String],
) -> (MmArc, EthCoin) {
    let (ctx, mut coin) = dev_eth_coin(coin_type, node, swap_contract);
    let fields = Arc::get_mut(&mut coin.0).expect("the coin is not shared yet");
    fields.swap_evidence = crate::swap_evidence::swap_evidence_policy_from("ETH", None, urls).unwrap();
    fields.web3_instances = urls
        .iter()
        .map(|url| Web3Instance {
            url: url.clone(),
            web3: crate::eth::alloy_compat::build_provider(
                crate::node_url::listed_nodes("TEST", vec![url.clone()]),
                vec![],
            )
            .unwrap(),
            is_parity: false,
        })
        .collect();
    (ctx, coin)
}

/// The payment call of the swap (`time_lock`, `secret_hash`) for `wei` base units, signed by the test key with the
/// account nonce `nonce` and not broadcast.
fn signed_unsent_payment(
    coin: &EthCoin,
    swap_contract: Address,
    time_lock: u32,
    secret_hash: &[u8],
    wei: U256,
    nonce: u64,
) -> SignedEthTx {
    let id = coin.etomic_swap_id(time_lock, secret_hash);
    let receiver = addr_from_raw_pubkey(test_dex_fee_addr_raw_pubkey()).unwrap();
    let (value, data) = match &coin.coin_type {
        EthCoinType::Erc20 { token_addr, .. } => {
            let call = SWAP_CONTRACT.function("erc20Payment").unwrap().encode_input(&[
                Token::FixedBytes(id),
                Token::Uint(wei),
                Token::Address(*token_addr),
                Token::Address(receiver),
                Token::FixedBytes(secret_hash.to_vec()),
                Token::Uint(U256::from(time_lock)),
            ]);
            (U256::zero(), call.unwrap())
        },
        _ => {
            let call = SWAP_CONTRACT.function("ethPayment").unwrap().encode_input(&[
                Token::FixedBytes(id),
                Token::Address(receiver),
                Token::FixedBytes(secret_hash.to_vec()),
                Token::Uint(U256::from(time_lock)),
            ]);
            (wei, call.unwrap())
        },
    };
    coin.sign_tx_for_send(UnSignedEthTx {
        nonce: nonce.into(),
        value,
        action: Action::Call(swap_contract),
        data,
        gas: 300_000.into(),
        gas_price: 1_000_000_000u64.into(),
    })
    .unwrap()
}

/// The already-sent question of the swap (`time_lock`, `secret_hash`), searched from the first block.
fn own_payment_sent(coin: &EthCoin, time_lock: u32, secret_hash: &[u8]) -> Result<Option<TransactionEnum>, String> {
    block_on(
        coin.check_if_my_payment_sent(time_lock, &[], &[], secret_hash, 0, &coin.swap_contract_address())
            .compat(),
    )
}

/// The read for a payment whose send ended with an error which kept the transaction `kept`.
fn sent_after_failed_send(
    coin: &EthCoin,
    kept: &TransactionEnum,
    time_lock: u32,
    secret_hash: &[u8],
) -> Result<Option<TransactionEnum>, String> {
    let read = coin
        .check_if_my_payment_sent_after_failed_send(kept, time_lock, secret_hash, 0, &coin.swap_contract_address())
        .expect("an EVM coin has the read for the payment call of the swap");
    block_on(read.compat())
}

/// Sends the maker payment of the swap (`time_lock`, `secret_hash`) and waits until it is mined.
fn send_mined_payment(node: &GethDev, coin: &EthCoin, time_lock: u32, secret_hash: &[u8]) -> TransactionEnum {
    let payment = coin
        .send_maker_payment(
            time_lock,
            &[],
            test_dex_fee_addr_raw_pubkey(),
            secret_hash,
            "0.001".parse().unwrap(),
            &coin.swap_contract_address(),
        )
        .wait()
        .unwrap();
    let payment_hash = format!("0x{}", hex::encode(payment.tx_hash().0));
    let receipt = node.wait_receipt(&payment_hash);
    assert_eq!(receipt["status"].as_str(), Some("0x1"), "payment tx reverted");
    payment
}

/// Asserts that `answer` is the transaction `payment`.
fn assert_found(answer: Result<Option<TransactionEnum>, String>, payment: &TransactionEnum) {
    let found = answer
        .expect("the read failed")
        .expect("the read did not find the payment");
    assert_eq!(found.tx_hash(), payment.tx_hash(), "the read found another transaction");
}

/// The reason of the read after a failed send while no node returns the transaction and its nonce is unused.
const NONCE_UNUSED: &str = "its nonce is unused, so it can still be mined";
/// The reason of an agreed read of which only one node answered.
const TOO_FEW_ANSWERS: &str = "fewer than two nodes answered";

/// Asserts that `answer` is an error with the undetermined mark and `reason` in its text: a read that cannot decide
/// is never "not sent".
fn assert_cannot_decide(answer: Result<Option<TransactionEnum>, String>, reason: &str) {
    let Err(text) = answer else {
        panic!("a read that cannot decide was answered as a decided one");
    };
    assert!(
        crate::swap_evidence::is_undetermined(&text),
        "the error does not carry the undetermined mark"
    );
    assert!(text.contains(reason), "the error does not name its reason");
}

/// The transaction count of the test account at the latest block: its next unused nonce.
fn next_unused_nonce(node: &GethDev) -> u64 {
    let count = node.rpc(
        "eth_getTransactionCount",
        json!([addr_hex(test_key_pair().address()), "latest"]),
    );
    u64::from_str_radix(count.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
}

/// For `coin` and the swap of `secret_hash`: before its payment is sent the already-sent question answers "not
/// sent". The read after a failed send, asked for a payment call that was signed with the account's next nonce and
/// never broadcast, cannot decide while that nonce is unused, and answers "not sent" once another transaction of the
/// account (the payment of the swap of `other_secret_hash`) has used the nonce. Once the payment is mined both
/// reads find it.
fn assert_own_payment_reads_follow_the_chain(
    node: &GethDev,
    coin: &EthCoin,
    swap_addr: Address,
    secret_hash: &[u8],
    other_secret_hash: &[u8],
) {
    let time_lock = (now_ms() / 1000) as u32 + 1000;
    let wei = U256::exp10(15);
    let nonce = next_unused_nonce(node);
    let unsent: TransactionEnum = signed_unsent_payment(coin, swap_addr, time_lock, secret_hash, wei, nonce).into();
    assert_eq!(own_payment_sent(coin, time_lock, secret_hash), Ok(None));
    assert_cannot_decide(
        sent_after_failed_send(coin, &unsent, time_lock, secret_hash),
        NONCE_UNUSED,
    );

    // Another transaction of the account takes the nonce: the unsent payment call can never be mined.
    send_mined_payment(node, coin, time_lock, other_secret_hash);
    assert!(
        next_unused_nonce(node) > nonce,
        "the nonce of the unsent payment call was not used"
    );
    assert_eq!(sent_after_failed_send(coin, &unsent, time_lock, secret_hash), Ok(None));

    let payment = send_mined_payment(node, coin, time_lock, secret_hash);
    assert_found(own_payment_sent(coin, time_lock, secret_hash), &payment);
    assert_found(sent_after_failed_send(coin, &payment, time_lock, secret_hash), &payment);
    // A later send of the same swap that kept its transaction: the payment the contract holds is the answer.
    assert_found(sent_after_failed_send(coin, &unsent, time_lock, secret_hash), &payment);
}

/// CRD ch.51, status note of 2026-11-28 after R29: on the dev chain, the already-sent question answers "not sent"
/// before the payment, the read after a failed send answers "not sent" only once the nonce of the unsent payment
/// call is used, and both find the payment afterwards, for ETH and for an ERC20 token.
#[test]
fn own_payment_reads_follow_an_eth_and_an_erc20_payment() {
    let node = match GethDev::start() {
        Some(node) => node,
        None => {
            log!("geth binary not available; skipping own_payment_reads_follow_an_eth_and_an_erc20_payment");
            return;
        },
    };

    let test_addr = test_key_pair().address();
    node.fund_eth(&addr_hex(test_addr), HUNDRED_ETH_WEI);
    let swap_addr: Address = node.deploy(ETOMIC_SWAP_BYTECODE, "").parse().unwrap();

    let (_eth_ctx, eth_coin) = dev_eth_coin(EthCoinType::Eth, &node, swap_addr);
    assert_own_payment_reads_follow_the_chain(&node, &eth_coin, swap_addr, &[2u8; 20], &[12u8; 20]);

    let token_addr = deploy_erc20_and_fund(&node, test_addr, U256::from(1000u64) * U256::exp10(18));
    let (_erc20_ctx, erc20_coin) = dev_eth_coin(
        EthCoinType::Erc20 {
            platform: "ETH".to_string(),
            token_addr,
        },
        &node,
        swap_addr,
    );
    assert_own_payment_reads_follow_the_chain(&node, &erc20_coin, swap_addr, &[3u8; 20], &[13u8; 20]);
}

/// CRD ch.51, status note of 2026-11-28 after R29: a coin whose reads must be agreed by two nodes while one of them
/// does not answer gets an undetermined error from both own-payment reads, before and after the payment, and never
/// "not sent". The same coin over two answering nodes gets "not sent" from the already-sent question before the
/// payment, "not sent" from the read after a failed send once the nonce of the unsent payment call is used, and the
/// payment after.
#[test]
fn own_payment_reads_wait_for_two_answering_nodes() {
    let node = match GethDev::start() {
        Some(node) => node,
        None => {
            log!("geth binary not available; skipping own_payment_reads_wait_for_two_answering_nodes");
            return;
        },
    };

    let test_addr = test_key_pair().address();
    node.fund_eth(&addr_hex(test_addr), HUNDRED_ETH_WEI);
    let swap_addr: Address = node.deploy(ETOMIC_SWAP_BYTECODE, "").parse().unwrap();

    // The dev node under its address and under its name counts as two hosts; nothing listens on the third URL.
    let by_address = node.rpc_url.clone();
    let by_name = by_address.replace("127.0.0.1", "localhost");
    let silent = format!("http://localhost:{}", crate::closed_port::CLOSED_PORT);
    let (_ctx, coin) = dev_eth_coin(EthCoinType::Eth, &node, swap_addr);
    let (_silent_ctx, one_silent) =
        dev_eth_coin_agreeing_over(EthCoinType::Eth, &node, swap_addr, &[by_address.clone(), silent]);
    let (_agreed_ctx, agreed) = dev_eth_coin_agreeing_over(EthCoinType::Eth, &node, swap_addr, &[by_address, by_name]);
    for with_policy in [&one_silent, &agreed] {
        assert!(
            matches!(
                with_policy.agreed_swap_reads(),
                Some(crate::SwapEvidencePolicy::TwoNodes { .. })
            ),
            "the coin's reads are not agreed by two nodes"
        );
    }

    let secret_hash = [4u8; 20];
    let time_lock = (now_ms() / 1000) as u32 + 1000;
    let nonce = next_unused_nonce(&node);
    let unsent: TransactionEnum =
        signed_unsent_payment(&coin, swap_addr, time_lock, &secret_hash, U256::exp10(15), nonce).into();

    // Two answering nodes agree on "not sent" for the already-sent question, and on a nonce that is unused.
    assert_eq!(own_payment_sent(&agreed, time_lock, &secret_hash), Ok(None));
    assert_cannot_decide(
        sent_after_failed_send(&agreed, &unsent, time_lock, &secret_hash),
        NONCE_UNUSED,
    );
    assert_cannot_decide(own_payment_sent(&one_silent, time_lock, &secret_hash), TOO_FEW_ANSWERS);
    assert_cannot_decide(
        sent_after_failed_send(&one_silent, &unsent, time_lock, &secret_hash),
        TOO_FEW_ANSWERS,
    );

    // Another transaction of the account takes the nonce: two answering nodes agree that the unsent payment call
    // can never be mined; one answering node still decides nothing.
    send_mined_payment(&node, &coin, time_lock, &[14u8; 20]);
    assert_eq!(
        sent_after_failed_send(&agreed, &unsent, time_lock, &secret_hash),
        Ok(None)
    );
    assert_cannot_decide(
        sent_after_failed_send(&one_silent, &unsent, time_lock, &secret_hash),
        TOO_FEW_ANSWERS,
    );

    let payment = send_mined_payment(&node, &coin, time_lock, &secret_hash);

    assert_cannot_decide(own_payment_sent(&one_silent, time_lock, &secret_hash), TOO_FEW_ANSWERS);
    assert_cannot_decide(
        sent_after_failed_send(&one_silent, &payment, time_lock, &secret_hash),
        TOO_FEW_ANSWERS,
    );
    assert_found(own_payment_sent(&agreed, time_lock, &secret_hash), &payment);
    assert_found(
        sent_after_failed_send(&agreed, &payment, time_lock, &secret_hash),
        &payment,
    );
}

/// Deterministic, offline coverage of the EVM `withdraw` path for both ETH and an
/// ERC20 token: build the withdrawal, broadcast it, and assert it executed and the
/// recipient actually received the funds on-chain.
#[test]
fn withdraw_eth_and_erc20() {
    let node = match GethDev::start() {
        Some(node) => node,
        None => {
            log!("geth binary not available; skipping withdraw_eth_and_erc20");
            return;
        },
    };

    let test_addr = test_key_pair().address();
    node.fund_eth(&addr_hex(test_addr), HUNDRED_ETH_WEI);

    // The swap contract is irrelevant to `withdraw`; deploy one so the coin is well-formed.
    let swap_addr: Address = node.deploy(ETOMIC_SWAP_BYTECODE, "").parse().unwrap();
    let recipient = "0x657980d55733B41c0C64c06003864e1aAD917Ca7";

    // --- ETH withdraw ---
    let (_eth_ctx, eth_coin) = dev_eth_coin(EthCoinType::Eth, &node, swap_addr);
    let details = eth_coin
        .withdraw(WithdrawRequest {
            memo: None,
            expiration_seconds: None,
            fee_method: None,
            gasless: None,
            coin: "ETH".into(),
            from: None,
            to: recipient.into(),
            amount: "1".parse().unwrap(),
            max: false,
            fee: None,
        })
        .wait()
        .unwrap();
    assert_eq!(details.to, vec![recipient.to_string()]);
    let tx_hash = eth_coin.send_raw_tx_bytes(&details.tx_hex.0).wait().unwrap();
    let receipt = node.wait_receipt(&format!("0x{}", tx_hash));
    assert_eq!(receipt["status"].as_str(), Some("0x1"), "eth withdraw tx reverted");
    // The recipient (starting from zero) now holds exactly 1 ETH.
    let bal = node.rpc("eth_getBalance", json!([recipient, "latest"]));
    assert_eq!(bal.as_str(), Some("0xde0b6b3a7640000"), "recipient eth balance"); // 1e18

    // --- ERC20 withdraw ---
    let token_addr = deploy_erc20_and_fund(&node, test_addr, U256::from(1000u64) * U256::exp10(18));
    let (_erc20_ctx, erc20_coin) = dev_eth_coin(
        EthCoinType::Erc20 {
            platform: "ETH".to_string(),
            token_addr,
        },
        &node,
        swap_addr,
    );
    let details = erc20_coin
        .withdraw(WithdrawRequest {
            memo: None,
            expiration_seconds: None,
            fee_method: None,
            gasless: None,
            coin: "TEST".into(),
            from: None,
            to: recipient.into(),
            amount: "100".parse().unwrap(),
            max: false,
            fee: None,
        })
        .wait()
        .unwrap();
    assert_eq!(details.to, vec![recipient.to_string()]);
    let tx_hash = erc20_coin.send_raw_tx_bytes(&details.tx_hex.0).wait().unwrap();
    let receipt = node.wait_receipt(&format!("0x{}", tx_hash));
    assert_eq!(receipt["status"].as_str(), Some("0x1"), "erc20 withdraw tx reverted");
    // The recipient now holds exactly 100 tokens.
    let balance_of = ERC20_CONTRACT
        .function("balanceOf")
        .unwrap()
        .encode_input(&[Token::Address(recipient.parse().unwrap())])
        .unwrap();
    let call = node.rpc(
        "eth_call",
        json!([{ "to": addr_hex(token_addr), "data": format!("0x{}", hex::encode(balance_of)) }, "latest"]),
    );
    let decoded = ERC20_CONTRACT
        .function("balanceOf")
        .unwrap()
        .decode_output(&hex::decode(call.as_str().unwrap().trim_start_matches("0x")).unwrap())
        .unwrap();
    match &decoded[0] {
        Token::Uint(bal) => assert_eq!(*bal, U256::from(100u64) * U256::exp10(18), "recipient token balance"),
        other => panic!("unexpected balanceOf output: {:?}", other),
    }
}
