//! # Purpose
//! The SOL legs of the legacy swap (CRD ch.40 §40.6D): the coin's swap state, the transaction type of a swap
//! message, and the inherent operations the `SwapOps`, `MarketCoinOps` and `MmCoin` methods of `SolanaCoin` call.
//!
//! # Rules implemented
//! R40.6D.2 (program check, reason), R40.6D.3, R40.6D.5, R40.6D.6 (payment, whole lamports), R40.6D.7 (sending),
//! R40.6D.8 (finality), R40.6D.9 (validation of the other side's payment), R40.6D.10 (claim and refund), R40.6D.11
//! (refund gate by cluster time), R40.6D.12 (spend search and secret), R40.6D.13 (dex fee), R40.6D.14 (fee
//! answers), R40.6D.16 (two-node evidence or the named node).
//!
//! # Invariants
//! - Every read that decides a refund or a secret step goes through `SolanaRpcPool::read_for_evidence`, and the
//!   two answers must be equal; a disagreement, fewer than two answers, or an unanswering named node is undetermined,
//!   and the caller waits or fails; it never refunds, claims, or answers "not spent" on it.
//! - Lamport arithmetic is checked.
//! - Nothing here puts a key-derived value into an error text.

use super::rpc_client::{RpcError, SolanaRpcClient};
use super::swap_decode::{decode_swap_tx, first_signature_of, top_level_instructions, SwapTxView};
use super::swap_program::{check_program_account, check_program_data, memo_instruction, ProgramCheck, SolCall,
                          SolTerms, WalletOnlyReason, MEMO_PROGRAM_ID};
use super::*;
use crate::{CanRefundHtlc, DexFee, HtlcPubkeyError, TradeFee, Transaction as CoinTransaction, SWAP_HTLC_PUBKEY_LEN};
use common::executor::Timer;
use kdf_crypto::sha256;
use solana_message::{Instruction, Message};
use solana_system_interface::instruction::SystemInstruction;
use std::time::Duration;

/// The size of a payment account's data: its deposit in the captured devnet payments is the node's rent-exempt
/// minimum for nine bytes (CRD ch.40 R40.6D.14, observed on public devnet transactions of the program).
pub(crate) const PAYMENT_ACCOUNT_SPACE: usize = 9;

/// The seconds a refund gate answers "wait" for when the cluster's time could not be read (R40.6D.11).
/// The scale of a lamport amount (nine decimals).
const LAMPORT_SCALE: i64 = 9;
const GATE_RETRY_SECS: u64 = 15;
/// The signatures a page asks for.
const SIGNATURE_PAGE: usize = 1000;
/// How long a validation tries again on an undetermined read, and how often (R40.6D.9).
const VALIDATION_DEADLINE: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// How long the taker's claim is retried (R40.6D.10; the chapter sets no figure).
const CLAIM_RETRY: Duration = Duration::from_secs(3600);
/// The maker's claim is sent only when the cluster's time is at least this many seconds before the lock time
/// (R40.6D.10).
const MAKER_CLAIM_MARGIN_SECS: u64 = 300;

/// What the activation found about the swap program of the coin's cluster (R40.6D.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SwapProgramState {
    /// The program passed the check: swaps are possible.
    Tradable(Pubkey),
    /// The coin and its tokens stay wallet-only, for this reason.
    WalletOnly(WalletOnlyReason),
}

/// The timing of the waits of the swap operations; tests shorten it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SwapTiming {
    pub(crate) poll: Duration,
    pub(crate) validation_deadline: Duration,
    /// How long the taker's claim is retried while the payment account is not known to be gone.
    pub(crate) claim_retry: Duration,
}

impl Default for SwapTiming {
    fn default() -> Self {
        SwapTiming {
            poll: POLL_INTERVAL,
            validation_deadline: VALIDATION_DEADLINE,
            claim_retry: CLAIM_RETRY,
        }
    }
}

/// The swap state of a Solana platform coin: its program check and the evidence node the activation named.
#[derive(Debug)]
pub struct SolanaSwapConfig {
    state: Mutex<SwapProgramState>,
    evidence_node: Mutex<Option<String>>,
    /// The swap-evidence policy fixed at activation (CRD ch.51 R100).
    evidence_policy: Mutex<crate::SwapEvidencePolicy>,
    timing: Mutex<SwapTiming>,
}

impl Default for SolanaSwapConfig {
    fn default() -> Self {
        SolanaSwapConfig {
            state: Mutex::new(SwapProgramState::WalletOnly(WalletOnlyReason::NoProgramConfigured)),
            evidence_node: Mutex::new(None),
            evidence_policy: Mutex::new(crate::SwapEvidencePolicy::NotAdopted),
            timing: Mutex::new(SwapTiming::default()),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> { mutex.lock().unwrap_or_else(|e| e.into_inner()) }

/// Terms of a payment used to price a transaction; they never reach a sent transaction (the type exposes only the
/// instructions that `fee_of` prices).
struct PricingTerms(SolTerms);

impl PricingTerms {
    fn payment_instruction(&self, program: &Pubkey) -> Instruction { self.0.payment_instruction(program) }

    fn spend_instruction(&self, program: &Pubkey) -> Instruction { self.0.spend_instruction(program, &[0u8; 32]) }

    fn refund_instruction(&self, program: &Pubkey) -> Instruction { self.0.refund_instruction(program) }
}

/// A Solana wire transaction of a swap message (R40.6D.9): the bytes as they are sent and its first signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SolanaSwapTx {
    wire: Vec<u8>,
    signature: String,
}

impl SolanaSwapTx {
    /// The signed transaction `tx` as a swap message.
    ///
    /// # Errors
    ///
    /// The transaction does not serialize or has no signature.
    pub fn from_signed(tx: &Transaction) -> Result<Self, String> {
        let wire = bincode::serialize(tx).map_err(|_| "the transaction does not serialize".to_owned())?;
        Self::from_wire(&wire)
    }

    /// The transaction of wire bytes.
    ///
    /// # Errors
    ///
    /// The bytes are not a Solana transaction, or it has no signature.
    pub fn from_wire(wire: &[u8]) -> Result<Self, String> {
        let signature = first_signature_of(wire)?;
        Ok(SolanaSwapTx {
            wire: wire.to_vec(),
            signature,
        })
    }

    /// The first signature in base58: the transaction's identity.
    pub fn signature(&self) -> &str { &self.signature }
}

impl CoinTransaction for SolanaSwapTx {
    fn tx_hex(&self) -> Vec<u8> { self.wire.clone() }

    /// The text of the first signature (base58), as bytes.
    fn tx_hash(&self) -> BytesJson { self.signature.as_bytes().to_vec().into() }
}

/// A swap key in the 33-byte field of the negotiation: the 32-byte public key, then one zero byte (R40.6D.5).
pub(crate) fn swap_key_field(key: &Pubkey) -> [u8; SWAP_HTLC_PUBKEY_LEN] {
    let mut field = [0u8; SWAP_HTLC_PUBKEY_LEN];
    field[..32].copy_from_slice(key.as_ref());
    field
}

/// The counterparty's swap key from its field: exactly the bound width, a zero last byte, and a point of the ed25519
/// curve in the first 32 bytes (R40.6D.5).
pub(crate) fn parse_swap_key(field: &[u8]) -> Result<Pubkey, MmError<HtlcPubkeyError>> {
    if field.len() != SWAP_HTLC_PUBKEY_LEN {
        return MmError::err(HtlcPubkeyError::UnexpectedLength(field.len()));
    }
    if field[SWAP_HTLC_PUBKEY_LEN - 1] != 0 {
        return MmError::err(HtlcPubkeyError::NotOnCurve(
            "ed25519",
            "the last byte of the field is not zero".to_owned(),
        ));
    }
    let bytes: [u8; 32] = field[..32]
        .try_into()
        .map_to_mm(|_| HtlcPubkeyError::UnexpectedLength(field.len()))?;
    if !solana_pubkey::bytes_are_curve_point(bytes) {
        return MmError::err(HtlcPubkeyError::NotOnCurve(
            "ed25519",
            "the key is not a point of the curve".to_owned(),
        ));
    }
    Ok(Pubkey::new_from_array(bytes))
}

/// `amount` as whole lamports: more decimals than the coin has is refused (R40.6D.6), as is a value that is not a
/// `u64` of lamports.
pub(crate) fn exact_lamports(amount: &BigDecimal) -> Result<u64, String> {
    let scaled = amount.with_scale(LAMPORT_SCALE);
    if &scaled != amount {
        return Err("The amount is not a whole number of lamports".to_owned());
    }
    crate::solana::solana_common::amount_to_units(amount, SOLANA_DEFAULT_DECIMALS as u8)
        .map_err(|_| "The amount does not fit the lamports of the coin".to_owned())
}

/// `amount` in lamports, rounded up (the dex fee's expected amount, R40.6D.13).
fn ceil_lamports(amount: &BigDecimal) -> Result<u64, String> {
    let truncated = amount.with_scale(LAMPORT_SCALE);
    let mut units = crate::solana::solana_common::amount_to_units(&truncated, SOLANA_DEFAULT_DECIMALS as u8)
        .map_err(|_| "The fee does not fit the lamports of the coin".to_owned())?;
    if &truncated < amount {
        units = units
            .checked_add(1)
            .ok_or_else(|| "The fee does not fit the lamports of the coin".to_owned())?;
    }
    Ok(units)
}

fn secret_32(bytes: &[u8]) -> Result<[u8; 32], String> {
    bytes.try_into().map_err(|_| "The secret is not 32 bytes".to_owned())
}

fn hash_32(bytes: &[u8]) -> Result<[u8; 32], String> {
    bytes
        .try_into()
        .map_err(|_| "The secret hash is not 32 bytes".to_owned())
}

/// What a node's read of a payment, for validation, found (R40.6D.9). Two nodes' views must be equal.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PaymentView {
    /// No finalized transaction of that signature.
    NotFinal,
    /// The answer does not decode, or is another transaction.
    Unreadable,
    /// The node's wire bytes for the signature are not the bytes the peer sent (R40.6D.9).
    BytesDiffer,
    /// Included with an error.
    Failed,
    /// No instruction of the program is this payment.
    NoMatchingInstruction,
    /// The payment account does not exist.
    AccountMissing,
    Account {
        owner: Pubkey,
        lamports: u64,
        rent_min: u64,
    },
}

enum Verdict {
    Valid,
    Invalid(String),
    Undetermined(String),
}

fn instruction_is_payment(ix: &super::swap_decode::ResolvedInstruction, program: &Pubkey, terms: &SolTerms) -> bool {
    ix.program == *program
        && matches!(
            SolCall::parse(&ix.data),
            Some(SolCall::Payment { secret_hash, lock_time, amount })
                if secret_hash == terms.secret_hash && lock_time == terms.lock_time && amount == terms.amount
        )
        && ix.accounts.first() == Some(&terms.sender)
        && ix.accounts.get(1) == Some(&terms.receiver)
}

/// Reads one transaction of `signature` and decodes it; a transaction the node lists but cannot give is a failed
/// read.
async fn read_view(node: &SolanaRpcClient, signature: &str) -> Result<SwapTxView, RpcError> {
    let answer = node
        .get_transaction_base64(signature)
        .await?
        .ok_or_else(|| RpcError::transport("getTransaction", "a listed transaction is not served"))?;
    decode_swap_tx(&answer, signature).map_err(|e| RpcError::transport("getTransaction", e))
}

/// The newest successful transaction of an address's history that `pick` accepts (R40.6D.12): signatures newest
/// first, the listing read to its first EMPTY page (a short page does not end it) and without a page limit, each
/// transaction read once. The listing ends at `until` (exclusive) when given. With `range` (`low`, `high`) only
/// listed signatures whose slot lies between the two (inclusive) are read.
async fn walk_history<T>(
    node: &SolanaRpcClient,
    address: &Pubkey,
    until: Option<&str>,
    range: Option<(u64, u64)>,
    pick: impl Fn(&SwapTxView) -> Option<T>,
) -> Result<Option<T>, RpcError> {
    let mut before: Option<String> = None;
    loop {
        let page = node
            .get_signatures_for_address_until(address, before.as_deref(), until, SIGNATURE_PAGE)
            .await?;
        let Some(last) = page.last() else {
            return Ok(None);
        };
        for info in &page {
            if info.err.is_some() || range.is_some_and(|(low, high)| info.slot < low || info.slot > high) {
                continue;
            }
            let view = read_view(node, &info.signature).await?;
            if view.failed {
                continue;
            }
            if let Some(found) = pick(&view) {
                return Ok(Some(found));
            }
        }
        // A node that answers the same page again would never reach the empty page: a failed read.
        if before.as_deref() == Some(last.signature.as_str()) {
            return Err(RpcError::transport(
                "getSignaturesForAddress",
                "the listing does not advance",
            ));
        }
        before = Some(last.signature.clone());
    }
}

/// One node's read of the payment account at `finalized`: the slot it answered at and whether the account exists
/// (R40.6D.12).
async fn read_account_state(node: &SolanaRpcClient, payment: &Pubkey) -> Result<(u64, bool), RpcError> {
    let (slot, account) = node.get_account_at_finalized(payment).await?;
    Ok((slot, account.is_some()))
}

/// One node's read of the payment address's listing once the account is gone: only the signatures whose slot lies
/// between `seen` (the last slot at which the account was seen; the payment transaction's own slot when none is
/// recorded) and `gone` (the first slot at which it was found gone), both inclusive, are fetched, and the listing
/// ends at the payment's own signature. The newest successful claim with the right secret, or refund, as wire bytes
/// with `true` for a claim.
async fn read_listing(
    node: &SolanaRpcClient,
    program: &Pubkey,
    terms: &SolTerms,
    payment_signature: &str,
    seen: Option<u64>,
    gone: u64,
) -> Result<Option<(bool, Vec<u8>)>, RpcError> {
    let (payment, _) = terms.payment_address(program);
    let low = match seen {
        Some(low) => low,
        None => {
            node.get_transaction_base64(payment_signature)
                .await?
                .ok_or_else(|| RpcError::transport("getTransaction", "the payment transaction is not served"))?
                .slot
        },
    };
    walk_history(node, &payment, Some(payment_signature), Some((low, gone)), |view| {
        view.instructions.iter().find_map(|ix| {
            if ix.program != *program || ix.accounts.get(2) != Some(&payment) {
                return None;
            }
            match SolCall::parse(&ix.data)? {
                SolCall::Spend { secret, .. } if sha256(&secret).to_vec() == terms.secret_hash => {
                    Some((true, view.wire.clone()))
                },
                SolCall::Refund { .. } => Some((false, view.wire.clone())),
                _ => None,
            }
        })
    })
    .await
}

async fn read_payment_view(
    node: &SolanaRpcClient,
    program: &Pubkey,
    signature: &str,
    peer_wire: &[u8],
    terms: &SolTerms,
) -> Result<PaymentView, RpcError> {
    let Some(answer) = node.get_transaction_base64(signature).await? else {
        return Ok(PaymentView::NotFinal);
    };
    let Ok(view) = decode_swap_tx(&answer, signature) else {
        return Ok(PaymentView::Unreadable);
    };
    if view.wire != peer_wire {
        return Ok(PaymentView::BytesDiffer);
    }
    if view.failed {
        return Ok(PaymentView::Failed);
    }
    if !view
        .instructions
        .iter()
        .any(|ix| instruction_is_payment(ix, program, terms))
    {
        return Ok(PaymentView::NoMatchingInstruction);
    }
    let (address, _) = terms.payment_address(program);
    match node.get_account_info_finalized(&address).await? {
        None => Ok(PaymentView::AccountMissing),
        Some(account) => {
            let rent_min = node.get_minimum_balance_for_rent_exemption(account.data.len()).await?;
            Ok(PaymentView::Account {
                owner: account.owner,
                lamports: account.lamports,
                rent_min,
            })
        },
    }
}

fn judge_payment(view: &PaymentView, program: &Pubkey, terms: &SolTerms) -> Verdict {
    match view {
        PaymentView::NotFinal => Verdict::Undetermined("the payment transaction is not final".to_owned()),
        PaymentView::Unreadable => Verdict::Invalid("the payment transaction could not be read".to_owned()),
        PaymentView::BytesDiffer => {
            Verdict::Invalid("the payment bytes are not the transaction the node has for the signature".to_owned())
        },
        PaymentView::Failed => Verdict::Invalid("the payment transaction failed".to_owned()),
        PaymentView::NoMatchingInstruction => {
            Verdict::Invalid("the transaction holds no payment with the agreed terms".to_owned())
        },
        PaymentView::AccountMissing => Verdict::Invalid("the payment account does not exist".to_owned()),
        PaymentView::Account {
            owner,
            lamports,
            rent_min,
        } => {
            if owner != program {
                return Verdict::Invalid("the payment account is not owned by the swap program".to_owned());
            }
            match terms.amount.checked_add(*rent_min) {
                Some(needed) if *lamports >= needed => Verdict::Valid,
                _ => Verdict::Invalid("the payment account holds less than the amount".to_owned()),
            }
        },
    }
}

/// The lamports a system transfer of the transaction moved from `from` to `to`, summed over its instructions.
fn transferred(view: &SwapTxView, from: &Pubkey, to: &Pubkey) -> u64 {
    view.instructions
        .iter()
        .filter(|ix| ix.program == solana_sdk_ids::system_program::ID)
        .filter(|ix| ix.accounts.first() == Some(from) && ix.accounts.get(1) == Some(to))
        .filter_map(|ix| match bincode::deserialize::<SystemInstruction>(&ix.data) {
            Ok(SystemInstruction::Transfer { lamports }) => Some(lamports),
            _ => None,
        })
        .fold(0u64, |total, lamports| total.saturating_add(lamports))
}

/// Which side of a swap pays, for the admission figures (R40.6D.14).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapSide {
    /// Sends a payment (and, for a taker, the taker fee).
    Sender { taker_fee: bool },
    /// Claims the other side's payment.
    Receiver,
}

impl SolanaCoin {
    /// The program the coin trades with: `Some` only when the activation's check passed (R40.6D.2).
    pub fn swap_program(&self) -> Option<Pubkey> {
        match &*lock(&self.swap.state) {
            SwapProgramState::Tradable(program) => Some(*program),
            SwapProgramState::WalletOnly(_) => None,
        }
    }

    /// Why the coin stays wallet-only, `None` when it can trade (R40.6D.2).
    pub fn swap_wallet_only_reason(&self) -> Option<WalletOnlyReason> {
        match &*lock(&self.swap.state) {
            SwapProgramState::Tradable(_) => None,
            SwapProgramState::WalletOnly(reason) => Some(reason.clone()),
        }
    }

    /// Records the evidence node the activation named (R40.6D.16): the matched entry of the node list.
    pub fn set_swap_evidence_node(&self, url: Option<String>) { *lock(&self.swap.evidence_node) = url; }

    /// Records the swap-evidence policy computed at activation (CRD ch.51 R100).
    pub fn set_swap_evidence_policy(&self, policy: crate::SwapEvidencePolicy) {
        *lock(&self.swap.evidence_policy) = policy;
    }

    /// The swap-evidence policy fixed at activation.
    pub(crate) fn stored_swap_evidence_policy(&self) -> crate::SwapEvidencePolicy {
        lock(&self.swap.evidence_policy).clone()
    }

    pub(crate) fn evidence_node(&self) -> Option<String> { lock(&self.swap.evidence_node).clone() }

    #[cfg(test)]
    pub(crate) fn set_swap_state_for_test(&self, state: SwapProgramState) { *lock(&self.swap.state) = state; }

    #[cfg(test)]
    pub(crate) fn set_swap_timing(&self, timing: SwapTiming) { *lock(&self.swap.timing) = timing; }

    fn timing(&self) -> SwapTiming { *lock(&self.swap.timing) }

    /// The program check of the activation (R40.6D.2): reads the program account once at `finalized` (and, for the
    /// upgradeable loader, its program-data header) from one node, and records the result. The activation succeeds
    /// whatever the result is.
    pub async fn check_swap_program(&self, configured: Option<Pubkey>) {
        let state = match configured {
            None => SwapProgramState::WalletOnly(WalletOnlyReason::NoProgramConfigured),
            Some(program) => match self.read_program_check(&program).await {
                Ok(()) => SwapProgramState::Tradable(program),
                Err(reason) => SwapProgramState::WalletOnly(reason),
            },
        };
        *lock(&self.swap.state) = state;
    }

    async fn read_program_check(&self, program: &Pubkey) -> Result<(), WalletOnlyReason> {
        let program = *program;
        let reads = self
            .rpc()
            .run(move |node| async move {
                let Some(account) = node.get_account_info_finalized(&program).await? else {
                    return Ok(Err(WalletOnlyReason::ProgramMissing));
                };
                match check_program_account(&account.owner, account.executable, &account.data) {
                    ProgramCheck::Passes => Ok(Ok(())),
                    ProgramCheck::Refused(reason) => Ok(Err(reason)),
                    ProgramCheck::ReadProgramData(address) => {
                        let Some(data) = node.get_account_header_finalized(&address, 45).await? else {
                            return Ok(Err(WalletOnlyReason::ProgramLoaderUnsupported));
                        };
                        Ok(check_program_data(&data.owner, &data.data))
                    },
                }
            })
            .await;
        reads.unwrap_or(Err(WalletOnlyReason::ProgramUnreadable))
    }

    fn require_program(&self) -> Result<Pubkey, String> {
        match &*lock(&self.swap.state) {
            SwapProgramState::Tradable(program) => Ok(*program),
            SwapProgramState::WalletOnly(reason) => Err(format!("{} cannot trade: {}", self.ticker, reason)),
        }
    }

    fn my_pubkey(&self) -> Pubkey { self.key_pair.pubkey() }

    /// The swap key of a field; the coin's own key is accepted in the same form.
    fn other_key(field: &[u8]) -> Result<Pubkey, String> {
        parse_swap_key(field).map_err(|e| e.into_inner().to_string())
    }

    /// The counterparty's swap key from its field: a key equal to the coin's own is refused (R40.6D.5).
    pub(crate) fn counterparty_key(&self, field: &[u8]) -> Result<Pubkey, String> {
        let key = Self::other_key(field)?;
        if key == self.my_pubkey() {
            return Err("The counterparty's swap key is the coin's own key".to_owned());
        }
        Ok(key)
    }

    /// `terms` of a payment whose wire bytes are at hand (not read from a node): the payment instruction of the
    /// program, checked against what the caller expects (R40.6D.10).
    fn terms_from_wire(
        &self,
        program: &Pubkey,
        wire: &[u8],
        sender: Option<&Pubkey>,
        receiver: Option<&Pubkey>,
        time_lock: u32,
    ) -> Result<SolTerms, String> {
        let instructions = top_level_instructions(wire)?;
        let terms = instructions
            .iter()
            .filter(|ix| ix.program == *program)
            .find_map(|ix| match SolCall::parse(&ix.data)? {
                SolCall::Payment {
                    secret_hash,
                    lock_time,
                    amount,
                } => Some(SolTerms {
                    sender: *ix.accounts.first()?,
                    receiver: *ix.accounts.get(1)?,
                    secret_hash,
                    lock_time,
                    amount,
                }),
                _ => None,
            })
            .ok_or_else(|| "The transaction holds no payment of the swap program".to_owned())?;
        if terms.lock_time != u64::from(time_lock) {
            return Err("The payment's lock time is not the swap's".to_owned());
        }
        if sender.is_some_and(|key| *key != terms.sender) {
            return Err("The payment's sender is not the expected key".to_owned());
        }
        if receiver.is_some_and(|key| *key != terms.receiver) {
            return Err("The payment's receiver is not the expected key".to_owned());
        }
        Ok(terms)
    }

    /// Builds and signs a transaction of `instructions` paid and signed by the coin's key (R40.6D.7), with a recent
    /// block hash of the node.
    async fn sign_instructions(
        &self,
        instructions: &[Instruction],
    ) -> Result<(Transaction, SolanaSwapTx), TransactionErr> {
        let plain = |e: String| TransactionErr::Plain(e);
        let hash = self
            .rpc()
            .get_latest_blockhash()
            .await
            .map_err(|e| plain(format!("{e}")))?;
        let payer = self.my_pubkey();
        let message = Message::new_with_blockhash(instructions, Some(&payer), &hash);
        let mut tx = Transaction::new_unsigned(message);
        tx.try_sign(&[&self.key_pair], hash)
            .map_err(|_| plain("The transaction could not be signed".to_owned()))?;
        let swap_tx = SolanaSwapTx::from_signed(&tx).map_err(plain)?;
        Ok((tx, swap_tx))
    }

    /// Sends a signed transaction, preflight on. A send that no node took keeps the signed transaction in the error.
    async fn broadcast(&self, tx: &Transaction, swap_tx: SolanaSwapTx) -> Result<SolanaSwapTx, TransactionErr> {
        match crate::solana::solana_market_ops::send_signed(self, tx).await {
            Ok(_) => Ok(swap_tx),
            Err(e) => Err(TransactionErr::TxRecoverable(swap_tx.into(), e)),
        }
    }

    /// Builds, signs and sends a transaction of `instructions` paid and signed by the coin's key (R40.6D.7).
    async fn send_instructions(&self, instructions: Vec<Instruction>) -> Result<SolanaSwapTx, TransactionErr> {
        let (tx, swap_tx) = self.sign_instructions(&instructions).await?;
        self.broadcast(&tx, swap_tx).await
    }

    /// Whether the node's simulation of the signed transaction succeeds. `Err`: the simulation could not be read.
    async fn simulation_succeeds(&self, tx: &Transaction) -> Result<bool, String> {
        let tx = tx.clone();
        self.rpc()
            .run(move |node| {
                let tx = tx.clone();
                async move { node.simulate_transaction(&tx).await }
            })
            .await
            .map(|error| error.is_none())
            .map_err(|e| format!("the simulation could not be read: {e}"))
    }

    /// The payment account at `finalized` by the evidence rule: `Ok(true)` present on both nodes (or the named one),
    /// `Ok(false)` gone on both, `Err` undetermined.
    async fn payment_account_present(&self, payment: &Pubkey) -> Result<bool, String> {
        let payment = *payment;
        let evidence = self.evidence_node();
        let ((_, first), (_, second)) = self
            .rpc()
            .read_for_evidence(evidence.as_deref(), |node| async move {
                read_account_state(&node, &payment).await
            })
            .await?;
        if first == second {
            Ok(first)
        } else {
            Err("two nodes disagree".to_owned())
        }
    }

    /// The cluster's time (the block time of the latest finalized slot) as both evidence readings: with the named
    /// node, one reading twice.
    async fn cluster_time_readings(&self) -> Result<(u64, u64), String> {
        let evidence = self.evidence_node();
        self.rpc()
            .read_for_evidence(evidence.as_deref(), |node| async move {
                let slot = node.get_finalized_slot().await?;
                let summary = node
                    .get_block_summary(slot)
                    .await?
                    .ok_or_else(|| RpcError::transport("getBlock", "the finalized block is not served"))?;
                summary
                    .block_time
                    .and_then(|time| u64::try_from(time).ok())
                    .ok_or_else(|| RpcError::transport("getBlock", "the finalized block has no time"))
            })
            .await
    }

    /// The cluster's time for the refund gate (R40.6D.11): the EARLIER of the two readings; the gate opens only when
    /// both readings have reached the lock time.
    async fn cluster_time_for_refund(&self) -> Result<u64, String> {
        let (first, second) = self.cluster_time_readings().await?;
        Ok(first.min(second))
    }

    /// The cluster's time for the maker's claim margin (R40.6D.10 condition (c)): the LATER of the two readings; a
    /// claim is sent only when both readings leave at least 300 s to the lock time.
    async fn cluster_time_for_claim_margin(&self) -> Result<u64, String> {
        let (first, second) = self.cluster_time_readings().await?;
        Ok(first.max(second))
    }

    /// The payment of `terms` that is on the chain, as wire bytes: the first successful payment instruction of the
    /// program in the history of the payment address (R40.6D.7). A failed read is an error.
    pub(crate) async fn find_payment_wire(
        &self,
        program: &Pubkey,
        terms: &SolTerms,
    ) -> Result<Option<Vec<u8>>, String> {
        let (address, _) = terms.payment_address(program);
        let (program, terms) = (*program, *terms);
        self.rpc()
            .run(move |node| async move {
                walk_history(&node, &address, None, None, |view| {
                    view.instructions
                        .iter()
                        .any(|ix| instruction_is_payment(ix, &program, &terms))
                        .then(|| view.wire.clone())
                })
                .await
            })
            .await
            .map_err(|e| format!("The payment could not be looked up: {e}"))
    }

    /// Sends the payment of `terms`; when it is already on the chain, that transaction is the answer and nothing is
    /// sent (R40.6D.7).
    pub(crate) async fn send_sol_payment(&self, terms: SolTerms) -> Result<SolanaSwapTx, TransactionErr> {
        let program = self.require_program().map_err(TransactionErr::Plain)?;
        match self.find_payment_wire(&program, &terms).await {
            Err(e) => Err(TransactionErr::Plain(e)),
            Ok(Some(wire)) => SolanaSwapTx::from_wire(&wire).map_err(TransactionErr::Plain),
            Ok(None) => self.send_instructions(vec![terms.payment_instruction(&program)]).await,
        }
    }

    /// The terms of a payment this node sends: it is the sender, the other side's key is `other_field`.
    pub(crate) fn my_payment_terms(
        &self,
        my_field: &[u8],
        other_field: &[u8],
        secret_hash: &[u8],
        time_lock: u32,
        amount: &BigDecimal,
    ) -> Result<SolTerms, String> {
        let mine = Self::other_key(my_field)?;
        if mine != self.my_pubkey() {
            return Err("The sending key is not the coin's key".to_owned());
        }
        Ok(SolTerms {
            sender: mine,
            receiver: self.counterparty_key(other_field)?,
            secret_hash: hash_32(secret_hash)?,
            lock_time: u64::from(time_lock),
            amount: exact_lamports(amount)?,
        })
    }

    /// The terms of the payment in `payment_wire` for a claim by this node, and the secret checked against its hash.
    fn claim_terms(
        &self,
        program: &Pubkey,
        payment_wire: &[u8],
        time_lock: u32,
        sender_field: &[u8],
        secret: &[u8],
    ) -> Result<(SolTerms, [u8; 32]), String> {
        let sender = self.counterparty_key(sender_field)?;
        let me = self.my_pubkey();
        let terms = self.terms_from_wire(program, payment_wire, Some(&sender), Some(&me), time_lock)?;
        let secret = secret_32(secret)?;
        if sha256(&secret).to_vec() != terms.secret_hash {
            return Err("The secret is not the payment's".to_owned());
        }
        Ok((terms, secret))
    }

    /// The maker's claim of the taker's payment, made with the maker's own secret (R40.6D.10). It is sent only when
    /// (a) the payment account is present at `finalized` by the evidence rule, (c) the cluster's time is at least
    /// 300 s before the lock time, and (b) the node's simulation of the claim succeeds; otherwise nothing is sent and
    /// the error names the condition.
    pub(crate) async fn claim_as_maker(
        &self,
        payment_wire: &[u8],
        time_lock: u32,
        sender_field: &[u8],
        secret: &[u8],
    ) -> Result<SolanaSwapTx, TransactionErr> {
        let plain = TransactionErr::Plain;
        let program = self.require_program().map_err(plain)?;
        let (terms, secret) = self
            .claim_terms(&program, payment_wire, time_lock, sender_field, secret)
            .map_err(plain)?;
        let (payment, _) = terms.payment_address(&program);
        match self.payment_account_present(&payment).await {
            Ok(true) => {},
            Ok(false) => {
                return Err(plain(
                    "Claim not sent, condition (a): the payment account is not present".to_owned(),
                ))
            },
            Err(e) => {
                return Err(plain(format!(
                    "Claim not sent, condition (a): the payment account could not be established: {e}"
                )))
            },
        }
        match self.cluster_time_for_claim_margin().await {
            Ok(time)
                if terms
                    .lock_time
                    .checked_sub(time)
                    .is_some_and(|left| left >= MAKER_CLAIM_MARGIN_SECS) => {},
            Ok(_) => {
                return Err(plain(
                    "Claim not sent, condition (c): fewer than 300 seconds remain to the lock time".to_owned(),
                ))
            },
            Err(e) => {
                return Err(plain(format!(
                    "Claim not sent, condition (c): the cluster time could not be established: {e}"
                )))
            },
        }
        let (tx, swap_tx) = self
            .sign_instructions(&[terms.spend_instruction(&program, &secret)])
            .await?;
        match self.simulation_succeeds(&tx).await {
            Ok(true) => {},
            Ok(false) => {
                return Err(plain(
                    "Claim not sent, condition (b): the simulation of the claim failed".to_owned(),
                ))
            },
            Err(e) => return Err(plain(format!("Claim not sent, condition (b): {e}"))),
        }
        self.broadcast(&tx, swap_tx).await
    }

    /// The taker's claim of the maker's payment, made with a secret already on the chain (R40.6D.10). It is sent
    /// unless the payment account is known to be gone at `finalized` (an undetermined read does not hold it back),
    /// after a successful simulation, before or after the lock time, and retried until the account is known to be
    /// gone or the retry time is out.
    pub(crate) async fn claim_as_taker(
        &self,
        payment_wire: &[u8],
        time_lock: u32,
        sender_field: &[u8],
        secret: &[u8],
    ) -> Result<SolanaSwapTx, TransactionErr> {
        let plain = TransactionErr::Plain;
        let program = self.require_program().map_err(plain)?;
        let (terms, secret) = self
            .claim_terms(&program, payment_wire, time_lock, sender_field, secret)
            .map_err(plain)?;
        let (payment, _) = terms.payment_address(&program);
        let timing = self.timing();
        let deadline = now_ms().saturating_add(u64::try_from(timing.claim_retry.as_millis()).unwrap_or(u64::MAX));
        loop {
            if let Ok(false) = self.payment_account_present(&payment).await {
                return Err(plain("Claim not sent: the payment account is gone".to_owned()));
            }
            let attempt = match self
                .sign_instructions(&[terms.spend_instruction(&program, &secret)])
                .await
            {
                Err(e) => e,
                Ok((tx, swap_tx)) => match self.simulation_succeeds(&tx).await {
                    Ok(true) => match self.broadcast(&tx, swap_tx).await {
                        Ok(sent) => return Ok(sent),
                        Err(e) => e,
                    },
                    Ok(false) => plain("Claim not sent: the simulation of the claim failed".to_owned()),
                    Err(e) => plain(format!("Claim not sent: {e}")),
                },
            };
            if now_ms() >= deadline {
                return Err(attempt);
            }
            Timer::sleep(timing.poll.as_secs_f64()).await;
        }
    }

    /// Refunds this node's own payment in `payment_wire` (R40.6D.10); this node is the sender. The refund gate is the
    /// caller's (R40.6D.11).
    pub(crate) async fn refund_payment(
        &self,
        payment_wire: &[u8],
        time_lock: u32,
        receiver_field: &[u8],
        secret_hash: &[u8],
    ) -> Result<SolanaSwapTx, TransactionErr> {
        let plain = TransactionErr::Plain;
        let program = self.require_program().map_err(plain)?;
        let receiver = self.counterparty_key(receiver_field).map_err(plain)?;
        let me = self.my_pubkey();
        let terms = self
            .terms_from_wire(&program, payment_wire, Some(&me), Some(&receiver), time_lock)
            .map_err(plain)?;
        if hash_32(secret_hash).map_err(plain)? != terms.secret_hash {
            return Err(plain("The secret hash is not the payment's".to_owned()));
        }
        self.send_instructions(vec![terms.refund_instruction(&program)]).await
    }

    /// Validates the other side's payment (R40.6D.9). Only the first signature of the peer's bytes is used; the
    /// transaction and the payment account are read from the nodes, two of them agreeing (R40.6D.16). An undetermined
    /// read is tried again until the deadline and never answers "valid".
    pub(crate) async fn validate_sol_payment(
        &self,
        wire: &[u8],
        time_lock: u32,
        sender_field: &[u8],
        receiver_field: &[u8],
        counterparty_is_sender: bool,
        secret_hash: &[u8],
        amount: &BigDecimal,
    ) -> Result<(), String> {
        let program = self.require_program()?;
        let (sender, receiver) = if counterparty_is_sender {
            (self.counterparty_key(sender_field)?, Self::other_key(receiver_field)?)
        } else {
            (Self::other_key(sender_field)?, self.counterparty_key(receiver_field)?)
        };
        let terms = SolTerms {
            sender,
            receiver,
            secret_hash: hash_32(secret_hash)?,
            lock_time: u64::from(time_lock),
            amount: exact_lamports(amount)?,
        };
        let signature = first_signature_of(wire)?;
        let timing = self.timing();
        let deadline =
            now_ms().saturating_add(u64::try_from(timing.validation_deadline.as_millis()).unwrap_or(u64::MAX));
        loop {
            let verdict = {
                let evidence = self.evidence_node();
                let reads = self
                    .rpc()
                    .read_for_evidence(evidence.as_deref(), |node| {
                        let signature = signature.clone();
                        async move { read_payment_view(&node, &program, &signature, wire, &terms).await }
                    })
                    .await;
                match reads {
                    Err(e) => Verdict::Undetermined(e),
                    Ok((first, second)) if first == second => judge_payment(&first, &program, &terms),
                    Ok(_) => Verdict::Undetermined("two nodes disagree about the payment".to_owned()),
                }
            };
            match verdict {
                Verdict::Valid => return Ok(()),
                Verdict::Invalid(reason) => return Err(reason),
                Verdict::Undetermined(reason) => {
                    if now_ms() >= deadline {
                        return Err(format!("The payment could not be established: {reason}"));
                    }
                    Timer::sleep(timing.poll.as_secs_f64()).await;
                },
            }
        }
    }

    /// The spend or refund of the payment of `terms` (R40.6D.12), with the two-node agreement of R40.6D.16.
    /// `payment_signature` is the payment's own signature (the listing's end and, without `seen`, the lower slot
    /// bound); `seen` is the last slot at which the payment account was seen. Besides the answer, the slot at which
    /// the account was seen when it still exists (the next call's `seen`).
    ///
    /// # Errors
    ///
    /// The read is undetermined (a failed read, a disagreement, fewer than two answers).
    pub(crate) async fn search_spend(
        &self,
        program: &Pubkey,
        terms: &SolTerms,
        payment_signature: &str,
        seen: Option<u64>,
    ) -> Result<(Option<FoundSwapTxSpend>, Option<u64>), String> {
        let (program, terms) = (*program, *terms);
        let (payment, _) = terms.payment_address(&program);
        let evidence = self.evidence_node();
        let undetermined = |e: String| format!("The spend of the payment could not be established: {e}");
        // Round one, the account: while it exists (two nodes or the named node agreeing) the answer is "not spent"
        // and no listed transaction is read.
        let ((slot_a, present_a), (slot_b, present_b)) = self
            .rpc()
            .read_for_evidence(evidence.as_deref(), |node| async move {
                read_account_state(&node, &payment).await
            })
            .await
            .map_err(undetermined)?;
        if present_a != present_b {
            return Err(undetermined("two nodes disagree".to_owned()));
        }
        if present_a {
            // The lower of the two slots is a slot at which both nodes saw the account.
            return Ok((None, Some(slot_a.min(slot_b))));
        }
        // Round two, the listing, once the account is gone: the later of the two slots is the upper bound.
        let gone = slot_a.max(slot_b);
        let (first, second) = self
            .rpc()
            .read_for_evidence(evidence.as_deref(), |node| async move {
                read_listing(&node, &program, &terms, payment_signature, seen, gone).await
            })
            .await
            .map_err(undetermined)?;
        if first != second {
            return Err(undetermined("two nodes disagree".to_owned()));
        }
        match first {
            Some((claimed, wire)) => {
                let tx = SolanaSwapTx::from_wire(&wire)?;
                Ok((
                    Some(if claimed {
                        FoundSwapTxSpend::Spent(tx.into())
                    } else {
                        FoundSwapTxSpend::Refunded(tx.into())
                    }),
                    None,
                ))
            },
            None => Ok((None, None)),
        }
    }

    /// The search for the spend of a payment given as wire bytes (R40.6D.12).
    pub(crate) async fn search_spend_of_payment(
        &self,
        payment_wire: &[u8],
        time_lock: u32,
        secret_hash: &[u8],
    ) -> Result<Option<FoundSwapTxSpend>, String> {
        let program = self.require_program()?;
        let terms = self.terms_from_wire(&program, payment_wire, None, None, time_lock)?;
        if hash_32(secret_hash)? != terms.secret_hash {
            return Err("The secret hash is not the payment's".to_owned());
        }
        let signature = first_signature_of(payment_wire)?;
        self.search_spend(&program, &terms, &signature, None)
            .await
            .map(|(found, _)| found)
    }

    /// Waits for the spend of the payment in `payment_wire` until `wait_until` (seconds), reading the payment account
    /// at `finalized` on every round, keeping the last slot at which it was seen, and fetching only the signatures
    /// between that slot and the slot at which it is gone (R40.6D.12).
    ///
    /// The answer is the transaction that spent the payment, whether a claim or a refund, exactly as the other coins'
    /// `wait_for_tx_spend` answers whatever spent the output. The only legacy caller is the taker's wait for the spend
    /// of its own payment (`taker_swap.rs`, `wait_for_tx_spend`): it passes the answer to `spend_secret`, and a spend
    /// that carries no secret is an extraction error, which ends the swap (`TakerPaymentWaitForSpendFailed`, then
    /// `Finish`) by the exit of CRD ch.51 R30 as the status note of 2026-11-22 under R30 states for the wait for the
    /// spend. A refunded payment has nothing left to refund, so the exit is the right one; ending the wait with an
    /// error instead would send the taker into a refund of a payment that is already gone. The maker's looks go
    /// through `search_for_swap_tx_spend_*`, which tell a claim from a refund.
    pub(crate) async fn wait_for_spend(&self, payment_wire: &[u8], wait_until: u64) -> Result<TransactionEnum, String> {
        let program = self.require_program()?;
        let payment_signature = first_signature_of(payment_wire)?;
        let instructions = top_level_instructions(payment_wire)?;
        let terms = instructions
            .iter()
            .filter(|ix| ix.program == program)
            .find_map(|ix| match SolCall::parse(&ix.data)? {
                SolCall::Payment {
                    secret_hash,
                    lock_time,
                    amount,
                } => Some(SolTerms {
                    sender: *ix.accounts.first()?,
                    receiver: *ix.accounts.get(1)?,
                    secret_hash,
                    lock_time,
                    amount,
                }),
                _ => None,
            })
            .ok_or_else(|| "The transaction holds no payment of the swap program".to_owned())?;
        let mut seen: Option<u64> = None;
        let poll = self.timing().poll;
        loop {
            match self.search_spend(&program, &terms, &payment_signature, seen).await {
                Ok((Some(FoundSwapTxSpend::Spent(tx) | FoundSwapTxSpend::Refunded(tx)), _)) => return Ok(tx),
                Ok((None, seen_now)) => seen = seen_now.or(seen),
                Err(_) => {},
            }
            if now_ms() / 1000 > wait_until {
                return Err("Waited too long for the spend of the payment".to_owned());
            }
            Timer::sleep(poll.as_secs_f64()).await;
        }
    }

    /// The secret of `secret_hash` in a spend transaction (R40.6D.12): the transaction is read from the nodes by its
    /// first signature, two agreeing, and its successful claim instruction of the program (top-level or inner) is
    /// read; the secret must hash to `secret_hash`.
    pub(crate) async fn secret_from_spend(&self, secret_hash: &[u8], spend_wire: &[u8]) -> Result<Vec<u8>, String> {
        let program = self.require_program()?;
        let hash = hash_32(secret_hash)?;
        let signature = first_signature_of(spend_wire)?;
        let evidence = self.evidence_node();
        let (first, second) = self
            .rpc()
            .read_for_evidence(evidence.as_deref(), |node| {
                let signature = signature.clone();
                async move {
                    let view = read_view(&node, &signature).await?;
                    if view.failed {
                        return Ok(None);
                    }
                    Ok(view.instructions.iter().find_map(|ix| match SolCall::parse(&ix.data)? {
                        SolCall::Spend { secret, .. } if ix.program == program && sha256(&secret).to_vec() == hash => {
                            Some(secret)
                        },
                        _ => None,
                    }))
                }
            })
            .await
            .map_err(|e| format!("The secret could not be established: {e}"))?;
        if first != second {
            return Err("The secret could not be established: two nodes disagree".to_owned());
        }
        first
            .map(|secret| secret.to_vec())
            .ok_or_else(|| "The transaction holds no claim with the swap's secret".to_owned())
    }

    /// The secret in the top-level claim instruction of wire bytes, without a node (the sync `extract_secret`).
    pub(crate) fn secret_from_wire(&self, secret_hash: &[u8], spend_wire: &[u8]) -> Result<Vec<u8>, String> {
        let program = self.require_program()?;
        let hash = hash_32(secret_hash)?;
        top_level_instructions(spend_wire)?
            .iter()
            .filter(|ix| ix.program == program)
            .find_map(|ix| match SolCall::parse(&ix.data)? {
                SolCall::Spend { secret, .. } if sha256(&secret).to_vec() == hash => Some(secret.to_vec()),
                _ => None,
            })
            .ok_or_else(|| "The transaction holds no claim with the swap's secret".to_owned())
    }

    /// The refund gate (R40.6D.11): the cluster's time, the block time of the latest finalized slot, read from two
    /// nodes (R40.6D.16). Refund only when both times have reached the lock; otherwise wait. A failed read waits.
    pub(crate) async fn refund_gate(&self, locktime: u64) -> CanRefundHtlc {
        match self.cluster_time_for_refund().await {
            Ok(time) if time >= locktime => CanRefundHtlc::CanRefundNow,
            Ok(time) => CanRefundHtlc::HaveToWait(locktime - time),
            Err(_) => CanRefundHtlc::HaveToWait(GATE_RETRY_SECS),
        }
    }

    /// Waits until the transaction in `wire` is included, successful and `finalized` (R40.6D.8). A transaction
    /// included with an error ends the wait with an error.
    pub(crate) async fn wait_until_final(&self, wire: &[u8], wait_until: u64, check_every: u64) -> Result<(), String> {
        let signature = first_signature_of(wire)?;
        loop {
            let read = self
                .rpc()
                .run(|node| {
                    let signature = signature.clone();
                    async move {
                        match node.get_transaction_base64(&signature).await? {
                            None => Ok(None),
                            Some(answer) => Ok(Some(answer.meta.map(|meta| meta.err.is_some()))),
                        }
                    }
                })
                .await;
            match read {
                Ok(Some(Some(false))) => return Ok(()),
                Ok(Some(Some(true))) => return Err("The transaction failed on the chain".to_owned()),
                Ok(Some(None)) | Ok(None) | Err(_) => {},
            }
            if now_ms() / 1000 > wait_until {
                return Err("Waited too long for the transaction to be final".to_owned());
            }
            Timer::sleep(check_every.max(1) as f64).await;
        }
    }

    /// The text of a swap's uuid for the memo (R40.6D.13): the standard hyphenated lowercase form of its 16 bytes.
    fn memo_text(uuid: &[u8]) -> Result<String, String> {
        uuid::Uuid::from_slice(uuid)
            .map(|uuid| uuid.hyphenated().to_string())
            .map_err(|_| "The swap's uuid is not 16 bytes".to_owned())
    }

    /// Sends the dex fee (R40.6D.13): a System transfer to the fee address and the memo of the swap's uuid.
    pub(crate) async fn send_sol_taker_fee(
        &self,
        dex_fee: &DexFee,
        fee_addr: &[u8],
        uuid: &[u8],
    ) -> Result<SolanaSwapTx, TransactionErr> {
        let plain = TransactionErr::Plain;
        let DexFee::Standard(amount) = dex_fee else {
            return Err(plain("Only the standard dex fee is sent for Solana coins".to_owned()));
        };
        let destination = Pubkey::new_from_array(
            <[u8; 32]>::try_from(fee_addr).map_err(|_| plain("The fee address is not 32 bytes".to_owned()))?,
        );
        let lamports = ceil_lamports(&amount.to_decimal()).map_err(plain)?;
        let memo = Self::memo_text(uuid).map_err(plain)?;
        let transfer = solana_system_interface::instruction::transfer(&self.my_pubkey(), &destination, lamports);
        self.send_instructions(vec![transfer, memo_instruction(&memo)]).await
    }

    /// Validates the dex fee (R40.6D.13).
    pub(crate) async fn validate_sol_fee(&self, args: ValidateFeeArgs<'_>) -> Result<(), String> {
        let TransactionEnum::SolanaTransaction(tx) = args.fee_tx else {
            return Err("The fee transaction is not a Solana transaction".to_owned());
        };
        let DexFee::Standard(amount) = args.dex_fee else {
            return Err("Only the standard dex fee is valid for Solana coins".to_owned());
        };
        let expected = ceil_lamports(&amount.to_decimal())?;
        let sender = match args.expected_sender.len() {
            32 => Pubkey::new_from_array(
                <[u8; 32]>::try_from(args.expected_sender).map_err(|_| "The sender key is not 32 bytes".to_owned())?,
            ),
            _ => self.counterparty_key(args.expected_sender)?,
        };
        let destination = Pubkey::new_from_array(
            <[u8; 32]>::try_from(args.fee_addr).map_err(|_| "The fee address is not 32 bytes".to_owned())?,
        );
        let memo = Self::memo_text(args.uuid)?;
        let signature = tx.signature().to_owned();
        let timing = self.timing();
        let deadline =
            now_ms().saturating_add(u64::try_from(timing.validation_deadline.as_millis()).unwrap_or(u64::MAX));
        loop {
            let read = self
                .rpc()
                .run(|node| {
                    let signature = signature.clone();
                    async move {
                        let Some(answer) = node.get_transaction_base64(&signature).await? else {
                            return Ok(None);
                        };
                        let view = decode_swap_tx(&answer, &signature)
                            .map_err(|e| RpcError::transport("getTransaction", e))?;
                        let height = node.get_block_summary(view.slot).await?.and_then(|b| b.block_height);
                        Ok(Some((view, height)))
                    }
                })
                .await;
            match read {
                Ok(Some((view, height))) => {
                    return judge_fee(
                        &view,
                        height,
                        &sender,
                        &destination,
                        expected,
                        &memo,
                        args.min_block_number,
                    )
                },
                Ok(None) | Err(_) => {
                    if now_ms() >= deadline {
                        return Err("The dex fee transaction could not be established".to_owned());
                    }
                    Timer::sleep(timing.poll.as_secs_f64()).await;
                },
            }
        }
    }

    /// The fee of a message of `instructions` paid by this coin's key, from one node (R40.6D.14).
    async fn fee_of(&self, instructions: Vec<Instruction>) -> Result<u64, String> {
        let payer = self.my_pubkey();
        self.rpc()
            .run(move |node| {
                let instructions = instructions.clone();
                async move {
                    let hash = node.get_latest_blockhash().await?;
                    let message = Message::new_with_blockhash(&instructions, Some(&payer), &hash);
                    node.get_fee_for_message(&message).await
                }
            })
            .await
            .map_err(|e| format!("The fee could not be read: {e}"))
    }

    /// Terms for pricing only: the type has no way into a sent transaction.
    fn pricing_terms(&self) -> PricingTerms {
        PricingTerms(SolTerms {
            sender: self.my_pubkey(),
            receiver: Pubkey::new_from_array([1u8; 32]),
            secret_hash: [0u8; 32],
            lock_time: 0,
            amount: 1,
        })
    }

    fn sol_fee(&self, lamports: u64) -> TradeFee {
        TradeFee {
            coin: self.ticker.clone(),
            amount: crate::solana::solana_common::lamports_to_sol(lamports).into(),
            paid_from_trading_vol: false,
        }
    }

    /// The payment's fee in lamports and the rent it deposits.
    async fn payment_fee_and_rent(&self, program: &Pubkey) -> Result<(u64, u64), String> {
        let fee = self
            .fee_of(vec![self.pricing_terms().payment_instruction(program)])
            .await?;
        let rent = self
            .rpc()
            .get_minimum_balance_for_rent_exemption(PAYMENT_ACCOUNT_SPACE)
            .await
            .map_err(|e| format!("The rent could not be read: {e}"))?;
        Ok((fee, rent))
    }

    /// The sender's fee answer (R40.6D.14): the payment's transaction fee plus the rent it deposits.
    pub(crate) async fn sol_sender_trade_fee(&self) -> Result<TradeFee, String> {
        let program = self.require_program()?;
        let (fee, rent) = self.payment_fee_and_rent(&program).await?;
        Ok(self.sol_fee(fee.checked_add(rent).ok_or("The fee overflows")?))
    }

    /// The receiver's fee answer (R40.6D.14): the claim's transaction fee.
    pub(crate) async fn sol_receiver_trade_fee(&self) -> Result<TradeFee, String> {
        let program = self.require_program()?;
        let fee = self
            .fee_of(vec![self.pricing_terms().spend_instruction(&program)])
            .await?;
        Ok(self.sol_fee(fee))
    }

    /// The taker fee's fee answer (R40.6D.14): the fee transaction's fee.
    pub(crate) async fn sol_taker_fee_trade_fee(&self) -> Result<TradeFee, String> {
        self.require_program()?;
        let me = self.my_pubkey();
        let transfer = solana_system_interface::instruction::transfer(&me, &me, 1);
        let fee = self
            .fee_of(vec![transfer, memo_instruction("00000000-0000-0000-0000-000000000000")])
            .await?;
        Ok(self.sol_fee(fee))
    }

    /// The SOL a node must hold to be admitted to a swap as `side` (R40.6D.14): the sending side's payment fee, its
    /// rent deposit and one refund's fee (and the taker fee's fee for a taker); the receiving side's claim fee.
    pub async fn swap_admission_lamports(&self, side: SwapSide) -> Result<u64, String> {
        let program = self.require_program()?;
        let overflow = || "The admission figure overflows".to_owned();
        match side {
            SwapSide::Receiver => {
                self.fee_of(vec![self.pricing_terms().spend_instruction(&program)])
                    .await
            },
            SwapSide::Sender { taker_fee } => {
                let (fee, rent) = self.payment_fee_and_rent(&program).await?;
                let refund = self
                    .fee_of(vec![self.pricing_terms().refund_instruction(&program)])
                    .await?;
                let mut total = fee
                    .checked_add(rent)
                    .and_then(|t| t.checked_add(refund))
                    .ok_or_else(overflow)?;
                if taker_fee {
                    let fee_fee = self.sol_taker_fee_trade_fee().await?;
                    let fee_fee = crate::solana::solana_common::amount_to_units(
                        &fee_fee.amount.to_decimal(),
                        SOLANA_DEFAULT_DECIMALS as u8,
                    )
                    .map_err(|_| overflow())?;
                    total = total.checked_add(fee_fee).ok_or_else(overflow)?;
                }
                Ok(total)
            },
        }
    }
}

/// Judges a dex fee transaction as the node answered it (R40.6D.13).
fn judge_fee(
    view: &SwapTxView,
    height: Option<u64>,
    sender: &Pubkey,
    destination: &Pubkey,
    expected: u64,
    memo: &str,
    min_block_number: u64,
) -> Result<(), String> {
    if view.failed {
        return Err("The dex fee transaction failed".to_owned());
    }
    if view.accounts.first() != Some(sender) {
        return Err("The dex fee was not paid by the taker's key".to_owned());
    }
    if transferred(view, sender, destination) < expected {
        return Err("The dex fee transaction moves less than the expected fee to the fee address".to_owned());
    }
    let memo_ok = view
        .instructions
        .iter()
        .any(|ix| ix.program == MEMO_PROGRAM_ID && ix.accounts.is_empty() && ix.data == memo.as_bytes());
    if !memo_ok {
        return Err("The dex fee transaction carries no memo of this swap".to_owned());
    }
    match height {
        Some(height) if height >= min_block_number => Ok(()),
        Some(_) => Err("The dex fee transaction is from before the swap's start".to_owned()),
        None => Err("The block height of the dex fee transaction is not known".to_owned()),
    }
}
