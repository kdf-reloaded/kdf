//! HTLC spend and already-sent detection over the lightwalletd transparent address index
//! (CRD ch.39 R39.11.4d, R39.11.4b(f), R39.11.17).
//!
//! The serving node files every indexed output under the pubkey-hash key type, so a P2SH (`b...`)
//! query is an empty success forever, while the redeem script's twenty bytes re-encoded under the
//! pubkey-hash version byte (an `R...` address) find the rows. Which key type a row carries depends
//! on the binary that indexed the block, so **both** forms are queried on every detection and neither
//! is assumed.
//!
//! The module has two layers. The decisions ([`decide_spend`], [`decide_sent`]) are pure functions
//! over what the endpoints returned, so every outcome can be driven without a network. The
//! orchestration ([`LightwalletdBackend::detect_output_spend`],
//! [`LightwalletdBackend::detect_funding`]) gathers that evidence endpoint by endpoint.
//!
//! The one rule everything else serves: **"could not tell" is never reported as "not spent" or "not
//! sent".** A failed call, a partial or gapped history stream, an undecodable transaction, and an
//! index that returns nothing for an output known to be mined are each their own error. The only
//! negative answers are:
//!
//! * the funding output is **listed as unspent** by the index under a form whose row carries the
//!   expected script; or
//! * the funding transaction is not mined at all and every read of a healthy endpoint was empty and
//!   clean.
//!
//! A history stream that merely contains the funding transaction and no spender is *not* a negative
//! answer: the service drops the newest height group of a stream (R39.11.4b), and the newest group of
//! an HTLC address is exactly where its spend lives.

use super::*;
use kdf_crypto::dhash160;
use kdf_crypto::ChecksumType;
use keys::hash::H160;
use keys::AddressFormat as UtxoAddressFormat;

/// The two address forms the same twenty bytes can be filed under (R39.11.4d).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum IndexEncoding {
    /// The `b...` form (version byte 85 on ARRR). The key type the lookup derives for it is
    /// empty on a node built with the defect.
    ScriptHash,
    /// The `R...` re-encoding (version byte 60 on ARRR): where the rows actually are.
    PubkeyHash,
}

/// How the coin encodes the two address forms of an HTLC.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HtlcAddressing {
    pub(crate) p2sh_prefix: u8,
    pub(crate) p2sh_t_prefix: u8,
    pub(crate) pubkey_prefix: u8,
    pub(crate) pubkey_t_prefix: u8,
    pub(crate) checksum_type: ChecksumType,
}

impl HtlcAddressing {
    /// The address of `encoding` for a redeem-script hash.
    pub(crate) fn address(&self, encoding: IndexEncoding, hash: [u8; 20]) -> Address {
        let (prefix, t_addr_prefix) = match encoding {
            IndexEncoding::ScriptHash => (self.p2sh_prefix, self.p2sh_t_prefix),
            IndexEncoding::PubkeyHash => (self.pubkey_prefix, self.pubkey_t_prefix),
        };
        Address {
            prefix,
            t_addr_prefix,
            hash: H160::from(hash).into(),
            checksum_type: self.checksum_type,
            hrp: None,
            addr_format: UtxoAddressFormat::Standard,
        }
    }
}

/// The scripts of a P2SH output: `OP_HASH160 <20 bytes> OP_EQUAL`.
pub(crate) fn p2sh_script_pubkey(hash: &[u8; 20]) -> Vec<u8> {
    let mut script = Vec::with_capacity(23);
    script.extend_from_slice(&[0xa9, 0x14]);
    script.extend_from_slice(hash);
    script.push(0x87);
    script
}

/// The redeem-script hash a P2SH script-pubkey commits to, or `None` for any other script. This is the
/// script validation of R39.11.4d(c) applied to the script the caller supplies.
pub(crate) fn p2sh_hash_of(script_pubkey: &[u8]) -> Option<[u8; 20]> {
    match script_pubkey {
        [0xa9, 0x14, hash @ .., 0x87] if hash.len() == 20 => hash.try_into().ok(),
        _ => None,
    }
}

/// The transparent output a detection is about.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OutputRef {
    /// Internal byte order.
    pub(crate) txid: [u8; 32],
    pub(crate) vout: u32,
    pub(crate) script_pubkey: Vec<u8>,
}

/// What is known about the funding transaction before the index is asked.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum FundingState {
    Mined(u64),
    /// The endpoint knows the transaction and reports no mined height.
    Unmined,
    /// The lookup failed. A fully spent transaction cannot be served without the node's transaction
    /// index, so this is not evidence of anything.
    Unknown,
}

/// Evidence from one address form on one endpoint. A failed read is carried as a failure, never as an
/// empty result.
#[derive(Clone, Debug)]
pub(crate) struct FormEvidence {
    pub(crate) server: String,
    pub(crate) encoding: IndexEncoding,
    pub(crate) utxos: Result<Vec<TransparentUtxo>, String>,
    pub(crate) history: Result<HistoryRead, String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FoundSpend {
    /// The spending transaction as the service returned it.
    pub(crate) tx: Vec<u8>,
    /// The input of `tx` that spends the output.
    pub(crate) input_index: usize,
    pub(crate) height: Option<u64>,
    pub(crate) server: String,
    pub(crate) encoding: IndexEncoding,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum NotSpentProof {
    /// The index lists the output, with the expected script, as unspent.
    UnspentOutputListed { server: String, encoding: IndexEncoding },
    /// The funding transaction is not mined, and a healthy endpoint returned nothing for it.
    FundingNotMined,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SpendOutcome {
    Spent(FoundSpend),
    NotSpent(NotSpentProof),
}

/// A funding transaction found on chain.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum FundingFound {
    /// Delivered by a history stream, bytes in hand.
    Transaction { tx: Vec<u8>, height: Option<u64> },
    /// Only the unspent listing named it; its bytes still have to be fetched by identifier.
    Listed { txid: [u8; 32] },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SentOutcome {
    Sent(FundingFound),
    /// A healthy endpoint returned nothing under either form, over a gap-free window.
    NotSent,
}

/// Why a detection could not reach an answer. Each variant is a named, reportable condition
/// (R39.11.17) and none of them is a negative answer.
#[derive(Clone, Debug, Display, PartialEq)]
pub(crate) enum HtlcIndexError {
    #[display(fmt = "the HTLC could not be searched for: {}", _0)]
    Lookup(String),
    /// Some reads failed, some streams were partial, or a transaction could not be decoded, and no
    /// endpoint gave a clean answer (R39.11.4b(f)). Gaps are the signature of an endpoint whose node
    /// cannot serve the shape of an HTLC spend (D39.11.g).
    #[display(fmt = "{}", _0)]
    Incomplete(String),
    /// The funding transaction is known (or presumed) mined and neither address form returned it
    /// (R39.11.4d(d)).
    #[display(
        fmt = "index-coverage failure: neither the script-hash nor the pubkey-hash form of the HTLC address returned \
               the funding transaction (funding {}); the address index cannot be relied on for this output",
        funding
    )]
    IndexCoverage { funding: String },
    /// Every admitted endpoint has been marked as keeping no address index (R39.11.4i(c)).
    #[display(
        fmt = "HTLC detection needs a server whose node keeps an address index (R39.11.17); no address index on: {}",
        _0
    )]
    NoAddressIndex(String),
    /// The index knows the output but no longer lists it, and no spending transaction was delivered.
    #[display(
        fmt = "the HTLC output is indexed but no longer listed as unspent, and its spending transaction was not \
               delivered; this is neither \"not spent\" nor a spend that can be read"
    )]
    SpenderNotDelivered,
}

impl HtlcIndexError {
    /// Whether the condition is one the sync-error channel carries (R39.8.0au): everything except an
    /// outright lookup failure, which already surfaces as a failed call.
    pub(crate) fn is_named_condition(&self) -> bool { !matches!(self, HtlcIndexError::Lookup(_)) }
}

// ---------------------------------------------------------------------------------------------
// Decisions
// ---------------------------------------------------------------------------------------------

/// Everything one endpoint's reads established.
#[derive(Default)]
struct EndpointFacts {
    problems: Vec<String>,
    gaps: Vec<u64>,
    spend: Option<FoundSpend>,
    listed: Option<(String, IndexEncoding)>,
    funding_seen: bool,
    /// A funding transaction delivered by history, with its height (sent detection).
    funded_by_history: Option<(Vec<u8>, Option<u64>, IndexEncoding)>,
    funded_by_listing: Option<[u8; 32]>,
    encodings_with_rows: Vec<IndexEncoding>,
}

fn group_by_server(evidence: &[FormEvidence]) -> Vec<(&str, Vec<&FormEvidence>)> {
    let mut groups: Vec<(&str, Vec<&FormEvidence>)> = Vec::new();
    for ev in evidence {
        match groups.iter_mut().find(|(server, _)| *server == ev.server) {
            Some((_, forms)) => forms.push(ev),
            None => groups.push((ev.server.as_str(), vec![ev])),
        }
    }
    groups
}

/// Reads one history transaction and returns its identifier, or a description of why it cannot be read.
fn decode(raw: &RawTx) -> Result<ZTransaction, String> {
    ZTransaction::read(raw.data.as_slice(), BranchId::Sapling)
        .map_err(|e| format!("cannot read a delivered transaction: {}", e))
}

fn has_output_with_script(tx: &ZTransaction, vout: Option<u32>, script: &[u8]) -> bool {
    let Some(bundle) = tx.transparent_bundle() else {
        return false;
    };
    match vout {
        Some(index) => bundle
            .vout
            .get(index as usize)
            .map_or(false, |out| out.script_pubkey().0 .0.as_slice() == script),
        None => bundle
            .vout
            .iter()
            .any(|out| out.script_pubkey().0 .0.as_slice() == script),
    }
}

fn collect_form(ev: &FormEvidence, facts: &mut EndpointFacts, output_script: &[u8], watched: Option<&OutputRef>) {
    let label = format!("{} {:?}", ev.server, ev.encoding);
    match &ev.utxos {
        Err(e) => facts.problems.push(format!("{} GetAddressUtxos failed: {}", label, e)),
        Ok(rows) => {
            for row in rows {
                // A row is the HTLC's only when its script is the expected one; the key alone is
                // twenty bytes shared by every output of that hash (R39.11.4d(c)).
                if row.script.as_slice() != output_script {
                    continue;
                }
                facts.encodings_with_rows.push(ev.encoding);
                match watched {
                    Some(output) if row.txid == output.txid && row.index == output.vout => {
                        facts.funding_seen = true;
                        facts.listed.get_or_insert((ev.server.clone(), ev.encoding));
                    },
                    Some(_) => {},
                    None => {
                        facts.funded_by_listing.get_or_insert(row.txid);
                    },
                }
            }
        },
    }
    match &ev.history {
        Err(e) => facts.problems.push(format!("{} history failed: {}", label, e)),
        Ok(read) => {
            if !read.gaps.is_empty() {
                facts.gaps.extend(read.gaps.iter().copied());
            }
            // A read stopped under R39.11.4h(b) reports its stop failure below; its gaps are artifacts of the
            // refusal and must not point at the transaction index (R39.11.4b(c) is the genuine-gap case).
            if !read.gaps.is_empty() && !read.stopped_on_unservable_heights {
                facts.problems.push(format!(
                    "{} history skipped height(s) {:?}: the node could not serve a transaction there (an HTLC spend \
                     has transparent inputs and no transparent outputs, which a node without its transaction index \
                     cannot serve)",
                    label, read.gaps
                ));
            }
            if let Some(failure) = &read.failure {
                facts
                    .problems
                    .push(format!("{} history stream ended in failure: {}", label, failure));
            }
            for raw in &read.transactions {
                let tx = match decode(raw) {
                    Ok(tx) => tx,
                    Err(e) => {
                        facts.problems.push(format!("{} {}", label, e));
                        continue;
                    },
                };
                let txid: [u8; 32] = *tx.txid().as_ref();
                match watched {
                    Some(output) => {
                        if txid == output.txid && has_output_with_script(&tx, Some(output.vout), output_script) {
                            facts.funding_seen = true;
                            facts.encodings_with_rows.push(ev.encoding);
                        }
                        let spender = tx.transparent_bundle().and_then(|bundle| {
                            bundle.vin.iter().position(|input| {
                                *input.prevout().hash() == output.txid && input.prevout().n() == output.vout
                            })
                        });
                        if let Some(input_index) = spender {
                            facts.spend.get_or_insert(FoundSpend {
                                tx: raw.data.clone(),
                                input_index,
                                height: raw.height,
                                server: ev.server.clone(),
                                encoding: ev.encoding,
                            });
                            facts.encodings_with_rows.push(ev.encoding);
                        }
                    },
                    None => {
                        if has_output_with_script(&tx, None, output_script) {
                            facts.encodings_with_rows.push(ev.encoding);
                            let better = match &facts.funded_by_history {
                                Some((_, Some(have), _)) => raw.height.map_or(false, |h| h < *have),
                                Some((_, None, _)) => raw.height.is_some(),
                                None => true,
                            };
                            if better {
                                facts.funded_by_history = Some((raw.data.clone(), raw.height, ev.encoding));
                            }
                        }
                    },
                }
            }
        },
    }
}

fn facts_of(forms: &[&FormEvidence], output_script: &[u8], watched: Option<&OutputRef>) -> EndpointFacts {
    let mut facts = EndpointFacts::default();
    for ev in forms {
        collect_form(ev, &mut facts, output_script, watched);
    }
    facts
}

fn incomplete(per_endpoint: &[EndpointFacts]) -> HtlcIndexError {
    let mut problems: Vec<String> = per_endpoint.iter().flat_map(|f| f.problems.iter().cloned()).collect();
    if problems.is_empty() {
        problems.push("no endpoint answered".to_owned());
    }
    HtlcIndexError::Incomplete(problems.join("; "))
}

/// Decides whether `output` has been spent from what the endpoints returned.
///
/// Positive evidence wins: a delivered transaction that spends the output is a spend whatever else
/// failed. Absent that, only a validated entry in the unspent listing, or an unmined funding
/// transaction seen by a healthy endpoint, is a negative answer. Everything else is an error.
pub(crate) fn decide_spend(
    output: &OutputRef,
    funding: FundingState,
    evidence: &[FormEvidence],
) -> Result<SpendOutcome, HtlcIndexError> {
    let per_endpoint: Vec<EndpointFacts> = group_by_server(evidence)
        .iter()
        .map(|(_, forms)| facts_of(forms, &output.script_pubkey, Some(output)))
        .collect();

    if let Some(spend) = per_endpoint.iter().find_map(|f| f.spend.clone()) {
        return Ok(SpendOutcome::Spent(spend));
    }
    if let Some((server, encoding)) = per_endpoint.iter().find_map(|f| f.listed.clone()) {
        return Ok(SpendOutcome::NotSpent(NotSpentProof::UnspentOutputListed {
            server,
            encoding,
        }));
    }
    let healthy: Vec<&EndpointFacts> = per_endpoint.iter().filter(|f| f.problems.is_empty()).collect();
    if healthy.is_empty() {
        return Err(incomplete(&per_endpoint));
    }
    // From here on every healthy endpoint answered every read, cleanly, and none lists the output.
    let funding_seen = per_endpoint.iter().any(|f| f.funding_seen);
    match (funding, funding_seen) {
        (FundingState::Unmined, false) => Ok(SpendOutcome::NotSpent(NotSpentProof::FundingNotMined)),
        (_, true) => Err(HtlcIndexError::SpenderNotDelivered),
        (known, false) => Err(HtlcIndexError::IndexCoverage {
            funding: match known {
                FundingState::Mined(height) => format!("mined at height {}", height),
                _ => "state unknown".to_owned(),
            },
        }),
    }
}

/// Decides whether a payment to `script_pubkey` already exists on chain.
///
/// A positive answer needs a delivered transaction with a transparent output of exactly that script,
/// or an unspent listing entry with exactly that script. "Not sent" needs a healthy endpoint: every
/// read answered and every history window complete.
pub(crate) fn decide_sent(script_pubkey: &[u8], evidence: &[FormEvidence]) -> Result<SentOutcome, HtlcIndexError> {
    let per_endpoint: Vec<EndpointFacts> = group_by_server(evidence)
        .iter()
        .map(|(_, forms)| facts_of(forms, script_pubkey, None))
        .collect();
    if let Some((tx, height, _)) = per_endpoint.iter().find_map(|f| f.funded_by_history.clone()) {
        return Ok(SentOutcome::Sent(FundingFound::Transaction { tx, height }));
    }
    if let Some(txid) = per_endpoint.iter().find_map(|f| f.funded_by_listing) {
        return Ok(SentOutcome::Sent(FundingFound::Listed { txid }));
    }
    if per_endpoint.iter().any(|f| f.problems.is_empty()) {
        Ok(SentOutcome::NotSent)
    } else {
        Err(incomplete(&per_endpoint))
    }
}

// ---------------------------------------------------------------------------------------------
// Orchestration
// ---------------------------------------------------------------------------------------------

/// How one detection reports its progress (R39.11.4h(d)): the coin it is for, and whether it is one
/// round of a polling wait, whose lines are repeated every few seconds and so go to `debug`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DetectionLog<'a> {
    pub(crate) coin: &'a str,
    pub(crate) polling: bool,
}

impl DetectionLog<'_> {
    fn line(&self, message: String) {
        if self.polling {
            common::log::debug!("{}", message);
        } else {
            common::log::info!("{}", message);
        }
    }
}

/// Evidence standing in for an endpoint that is skipped for having no address index: a failure, so the
/// endpoint is undetermined and the undetermined error lists it (R39.11.4i(b)).
fn skipped_no_address_index(server: &str, node_message: &str) -> FormEvidence {
    let reason = format!("skipped: no address index (node message: {})", node_message);
    FormEvidence {
        server: server.to_owned(),
        encoding: IndexEncoding::PubkeyHash,
        utxos: Err(reason.clone()),
        history: Err(reason),
    }
}

/// Runs `fut` unless `deadline` has passed or passes first; `None` is the endpoint's time budget
/// (R39.11.4h(c)) being used up.
async fn within_budget<T>(deadline: tokio::time::Instant, fut: impl std::future::Future<Output = T>) -> Option<T> {
    if tokio::time::Instant::now() >= deadline {
        return None;
    }
    tokio::time::timeout_at(deadline, fut).await.ok()
}

fn describe_spend_decision(result: &Result<SpendOutcome, HtlcIndexError>) -> String {
    match result {
        Ok(SpendOutcome::Spent(spend)) => format!(
            "spent: found on {} under the {:?} form, input {}, height {:?}",
            spend.server, spend.encoding, spend.input_index, spend.height
        ),
        Ok(SpendOutcome::NotSpent(proof)) => format!("not spent ({:?})", proof),
        Err(e) => format!("undetermined: {}", e),
    }
}

fn describe_sent_decision(result: &Result<SentOutcome, HtlcIndexError>) -> String {
    match result {
        Ok(SentOutcome::Sent(FundingFound::Transaction { height, .. })) => {
            format!("sent: the payment transaction was delivered, height {:?}", height)
        },
        Ok(SentOutcome::Sent(FundingFound::Listed { txid })) => format!(
            "sent: the unspent listing names transaction {:?}",
            txid_internal_to_display(*txid)
        ),
        Ok(SentOutcome::NotSent) => "not sent".to_owned(),
        Err(e) => format!("undetermined: {}", e),
    }
}

impl LightwalletdBackend {
    /// Reads both address forms of `hash` on `server`, within the endpoint's time budget
    /// (R39.11.4h(c)). A failed read is recorded as a failure, never as an empty result.
    async fn form_evidence(
        &self,
        server: &str,
        hash: [u8; 20],
        from: u64,
        to: u64,
        log: &DetectionLog<'_>,
    ) -> Vec<FormEvidence> {
        let started = tokio::time::Instant::now();
        let deadline = started + self.endpoint_budget;
        let budget = self.endpoint_budget;
        let mut out = Vec::with_capacity(2);
        for encoding in [IndexEncoding::PubkeyHash, IndexEncoding::ScriptHash] {
            let address = self.addressing.address(encoding, hash).to_string();
            self.note_progress(
                hash,
                format!("{}, {:?} form, GetAddressUtxos of {}", server, encoding, address),
            );
            let utxos = within_budget(deadline, self.transport.address_utxos(server, &address))
                .await
                .map_or_else(
                    || {
                        Err(format!(
                            "lightwalletd {}: {:?} form, GetAddressUtxos of {}: the {:?} time budget for this \
                             detection on this endpoint was used up",
                            server, encoding, address, budget
                        ))
                    },
                    |result| result.map_err(|e| e.to_string()),
                );
            let history = {
                let transport = self.transport.clone();
                let (history_address, history_server) = (address.clone(), server.to_owned());
                read_history(
                    from,
                    to,
                    TADDR_HISTORY_WINDOW_BLOCKS,
                    HistoryLimits::HTLC,
                    move |start, end| {
                        self.note_progress(
                            hash,
                            format!(
                                "{}, {:?} form, history of {} blocks {}..={}",
                                history_server, encoding, history_address, start, end
                            ),
                        );
                        let transport = transport.clone();
                        let address = history_address.clone();
                        let server = history_server.clone();
                        async move {
                            match within_budget(deadline, transport.taddr_txids(&server, &address, start, end)).await {
                                Some(read) => read,
                                None => WindowRead {
                                    txs: Vec::new(),
                                    error: Some(WindowError {
                                        serve_failure: false,
                                        message: format!(
                                            "the {:?} time budget for this detection on this endpoint was used up in \
                                         the history read of blocks {}..={}",
                                            budget, start, end
                                        ),
                                    }),
                                },
                            }
                        }
                    },
                )
                .await
                .map(|mut read| {
                    read.failure = read.failure.map(|failure| {
                        format!(
                            "lightwalletd {}: {:?} form, history of {} stopped: {}",
                            server, encoding, address, failure
                        )
                    });
                    read
                })
                .map_err(|e| e.to_string())
            };
            let summary = match (&utxos, &history) {
                (Ok(rows), Ok(read)) => format!(
                    "{} transaction(s), {} unspent row(s), {} gap(s), {} request(s)",
                    read.transactions.len(),
                    rows.len(),
                    read.gaps.len(),
                    read.requests
                ),
                (Err(e), Ok(read)) => format!(
                    "{} transaction(s), unspent listing failed ({}), {} gap(s), {} request(s)",
                    read.transactions.len(),
                    e,
                    read.gaps.len(),
                    read.requests
                ),
                (_, Err(e)) => format!("history read failed: {}", e),
            };
            let stopped = history
                .as_ref()
                .ok()
                .and_then(|read| read.failure.as_deref())
                .map_or_else(String::new, |failure| format!("; stopped: {}", failure));
            log.line(format!(
                "HTLC detection for {}: {} {:?} form: {}, {} ms elapsed on this endpoint{}",
                log.coin,
                server,
                encoding,
                summary,
                started.elapsed().as_millis(),
                stopped
            ));
            out.push(FormEvidence {
                server: server.to_owned(),
                encoding,
                utxos,
                history,
            });
        }
        out
    }

    /// Marks `server` "no address index" if one of the reads in `forms` failed with the node's message
    /// for it (R39.11.4i). The mark lasts as long as this backend does: a new activation builds a new one,
    /// and re-evaluating admission does not touch it.
    fn mark_if_no_address_index(&self, server: &str, forms: &[FormEvidence]) {
        let message = forms.iter().find_map(|ev| {
            let failures = [
                ev.utxos.as_ref().err().map(String::as_str),
                ev.history.as_ref().err().map(String::as_str),
                ev.history.as_ref().ok().and_then(|read| read.failure.as_deref()),
            ];
            failures
                .into_iter()
                .flatten()
                .find(|text| mentions_no_address_index(text))
        });
        let Some(message) = message else {
            return;
        };
        let first = self
            .no_address_index
            .lock()
            .insert(server.to_owned(), message.to_owned())
            .is_none();
        if first {
            common::log::warn!(
                "lightwalletd {} keeps no address index and is skipped by HTLC detection for the rest of this \
                 activation; the node said: {}",
                server,
                message
            );
        }
    }

    fn no_address_index_message(&self, server: &str) -> Option<String> {
        self.no_address_index.lock().get(server).cloned()
    }

    /// R39.11.4i(c): the detection cannot succeed when every admitted endpoint is marked.
    fn all_without_address_index(&self, admitted: &[String]) -> Result<(), HtlcIndexError> {
        let marks = self.no_address_index.lock();
        if !admitted.is_empty() && admitted.iter().all(|server| marks.contains_key(server)) {
            let listed: Vec<String> = admitted
                .iter()
                .map(|server| format!("{} (node message: {})", server, marks[server]))
                .collect();
            return Err(HtlcIndexError::NoAddressIndex(listed.join(", ")));
        }
        Ok(())
    }

    /// Records what the detection of the HTLC with redeem-script hash `hash` is reading, for the
    /// recovery bound's error (R39.5.5). Replaced on every read; removed when the detection ends.
    fn note_progress(&self, hash: [u8; 20], what: String) { self.detection_progress.lock().insert(hash, what); }

    /// What the detection in flight for the HTLC with this redeem-script hash read last. After a
    /// detection was dropped before it ended, this stays as it was (R39.5.5).
    pub(crate) fn detection_progress(&self, hash: &[u8; 20]) -> Option<String> {
        self.detection_progress.lock().get(hash).cloned()
    }

    /// Remembers, per endpoint, which form returned rows (R39.11.4d(b)). It never narrows what is
    /// probed next time: both forms are queried on every detection.
    fn note_encodings(&self, server: &str, forms: &[FormEvidence]) {
        let answered = forms
            .iter()
            .filter(|ev| ev.server == server)
            .find(|ev| {
                ev.utxos.as_ref().map_or(false, |rows| !rows.is_empty())
                    || ev.history.as_ref().map_or(false, |h| !h.transactions.is_empty())
            })
            .map(|ev| ev.encoding);
        if let Some(encoding) = answered {
            let previous = self.index_encodings.lock().insert(server.to_owned(), encoding);
            if previous != Some(encoding) {
                common::log::info!(
                    "lightwalletd {} answers HTLC address queries under the {:?} form (was {:?})",
                    server,
                    encoding,
                    previous
                );
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn answering_encoding(&self, server: &str) -> Option<IndexEncoding> {
        self.index_encodings.lock().get(server).copied()
    }

    async fn funding_state(&self, txid: [u8; 32]) -> FundingState {
        match self.transaction(txid).await {
            Ok(raw) => raw.height.map_or(FundingState::Unmined, FundingState::Mined),
            Err(e) => {
                common::log::debug!("the funding transaction could not be looked up: {}", e);
                FundingState::Unknown
            },
        }
    }

    /// The history window of a detection, from the Sapling floor (below which no block is read).
    ///
    /// A spend cannot precede its funding, so when the funding transaction is reported mined the
    /// window starts at its height, whatever the caller's start was, and reaches it even when it is
    /// above the tip (R39.11.4h(a)). Without a funding height the window starts at the caller's
    /// start and runs to the tip.
    fn history_window(&self, from_block: u64, tip: u64, funding: Option<FundingState>) -> (u64, u64) {
        let floor = self.sapling_activation_height().max(1);
        match funding {
            Some(FundingState::Mined(height)) => {
                let from = height.max(floor);
                (from, tip.max(height).max(from))
            },
            _ => {
                let from = from_block.max(floor);
                (from, tip.max(from))
            },
        }
    }

    /// HTLC spend detection (R39.11.4d). `from_block` is the caller's search start; it applies only
    /// while the funding height is not known (R39.11.4h(a)).
    ///
    /// Endpoints are tried in configured order and the first conclusive answer is returned; an
    /// endpoint that fails the methods needed, stops under a limit of R39.11.4h or uses up its time
    /// budget is not an answer, and the next is tried.
    pub(crate) async fn detect_output_spend(
        &self,
        output: &OutputRef,
        from_block: u64,
        log: &DetectionLog<'_>,
    ) -> Result<SpendOutcome, HtlcIndexError> {
        let hash = p2sh_hash_of(&output.script_pubkey).ok_or_else(|| {
            HtlcIndexError::Lookup("the watched output's script-pubkey is not pay-to-script-hash".to_owned())
        })?;
        self.note_progress(hash, "endpoint admission, chain tip and funding lookup".to_owned());
        let result = self.detect_output_spend_of(output, hash, from_block, log).await;
        // Removed only when the detection ends; a detection dropped by a deadline leaves its last
        // read for the error that reports the deadline (R39.5.5).
        self.detection_progress.lock().remove(&hash);
        result
    }

    async fn detect_output_spend_of(
        &self,
        output: &OutputRef,
        hash: [u8; 20],
        from_block: u64,
        log: &DetectionLog<'_>,
    ) -> Result<SpendOutcome, HtlcIndexError> {
        let admitted = self
            .admitted()
            .await
            .map_err(|e| HtlcIndexError::Lookup(e.to_string()))?;
        self.all_without_address_index(&admitted)?;
        let tip = self
            .chain_tip()
            .await
            .map_err(|e| HtlcIndexError::Lookup(e.to_string()))?;
        let funding = self.funding_state(output.txid).await;
        let (from, to) = self.history_window(from_block, tip, Some(funding));
        log.line(format!(
            "HTLC spend detection for {}: output {:?}:{}, funding {:?}, history blocks {}..={}, {} admitted endpoint(s)",
            log.coin,
            txid_internal_to_display(output.txid),
            output.vout,
            funding,
            from,
            to,
            admitted.len()
        ));

        let mut evidence = Vec::new();
        for server in admitted {
            if let Some(message) = self.no_address_index_message(&server) {
                log.line(format!(
                    "HTLC spend detection for {}: {} skipped: no address index",
                    log.coin, server
                ));
                evidence.push(skipped_no_address_index(&server, &message));
                continue;
            }
            let forms = self.form_evidence(&server, hash, from, to, log).await;
            self.mark_if_no_address_index(&server, &forms);
            self.note_encodings(&server, &forms);
            evidence.extend(forms);
            if decide_spend(output, funding, &evidence).is_ok() {
                break;
            }
        }
        let result = decide_spend(output, funding, &evidence);
        log.line(format!(
            "HTLC spend detection for {}: {}{}",
            log.coin,
            if result.is_ok() {
                "decision "
            } else {
                "no endpoint gave an answer, "
            },
            describe_spend_decision(&result)
        ));
        result
    }

    /// Already-sent detection (R39.11.4d): is there a payment to `script_pubkey` on chain?
    pub(crate) async fn detect_funding(
        &self,
        script_pubkey: &[u8],
        from_block: u64,
        log: &DetectionLog<'_>,
    ) -> Result<SentOutcome, HtlcIndexError> {
        let hash = p2sh_hash_of(script_pubkey)
            .ok_or_else(|| HtlcIndexError::Lookup("the HTLC script-pubkey is not pay-to-script-hash".to_owned()))?;
        self.note_progress(hash, "endpoint admission and chain tip".to_owned());
        let result = self.detect_funding_of(script_pubkey, hash, from_block, log).await;
        self.detection_progress.lock().remove(&hash);
        result
    }

    async fn detect_funding_of(
        &self,
        script_pubkey: &[u8],
        hash: [u8; 20],
        from_block: u64,
        log: &DetectionLog<'_>,
    ) -> Result<SentOutcome, HtlcIndexError> {
        let admitted = self
            .admitted()
            .await
            .map_err(|e| HtlcIndexError::Lookup(e.to_string()))?;
        self.all_without_address_index(&admitted)?;
        let tip = self
            .chain_tip()
            .await
            .map_err(|e| HtlcIndexError::Lookup(e.to_string()))?;
        let (from, to) = self.history_window(from_block, tip, None);
        log.line(format!(
            "HTLC already-sent detection for {}: payment script {}, funding transaction not known, history blocks \
             {}..={}, {} admitted endpoint(s)",
            log.coin,
            hex::encode(script_pubkey),
            from,
            to,
            admitted.len()
        ));

        let mut evidence = Vec::new();
        for server in admitted {
            if let Some(message) = self.no_address_index_message(&server) {
                log.line(format!(
                    "HTLC already-sent detection for {}: {} skipped: no address index",
                    log.coin, server
                ));
                evidence.push(skipped_no_address_index(&server, &message));
                continue;
            }
            let forms = self.form_evidence(&server, hash, from, to, log).await;
            self.mark_if_no_address_index(&server, &forms);
            self.note_encodings(&server, &forms);
            evidence.extend(forms);
            if decide_sent(script_pubkey, &evidence).is_ok() {
                break;
            }
        }
        let result = decide_sent(script_pubkey, &evidence);
        log.line(format!(
            "HTLC already-sent detection for {}: {}{}",
            log.coin,
            if result.is_ok() {
                "decision "
            } else {
                "no endpoint gave an answer, "
            },
            describe_sent_decision(&result)
        ));
        result
    }
}

#[cfg(test)] mod tests;
