//! The KDF transaction assembler (CRD ch.39 R39.6.5d and the HTLC transaction rules).
//!
//! The published Zcash transaction builder cannot express four things this project's shielded coins
//! need, so the transaction is assembled here from the libraries' public lower-level pieces instead of
//! from a patched copy of the builder:
//!
//! - a transparent input that spends a KDF atomic-swap P2SH output, with the script signature
//!   `<signature> <selector or secret> <redeem script>` and a caller-chosen sequence number;
//! - an explicit legacy `nLockTime`;
//! - transparent outputs kept exactly as given (count, order, value, script);
//! - an output-only Ironwood bundle (spends disabled, empty-tree anchor, default padding), as the
//!   dictated chain's own wallets build a payment from Sapling into Ironwood.
//!
//! Everything else follows what the published builder does: a Sapling bundle from the `sapling-crypto`
//! builder, proofs before signatures, the shielded signature hash of the transaction for the Sapling
//! and Ironwood signatures, the transparent signature hash with the redeem script as script code, and
//! the fee check against a [`FeeRule`] (a fixed fee today; the ZIP 317 rule works too, see
//! [`kdf_p2sh_input_size`]). The expiry height is explicit ([`Z_TX_EXPIRY_DELTA`]), so a later change
//! of a library default cannot move it.
//!
//! Nothing here panics: every failure is an [`AssemblerError`].

use rand_core::{CryptoRng, Rng};
use sapling::builder as sapling_builder;
use sapling::keys::{FullViewingKey, OutgoingViewingKey};
use sapling::prover::{OutputProver, SpendProver};
use sapling::value::NoteValue as SaplingNoteValue;
use sapling::zip32::ExtendedSpendingKey;
use sapling::{MerklePath, Note, PaymentAddress};
use std::fmt;
use zcash_primitives::transaction::builder::cached_orchard_proving_key;
use zcash_primitives::transaction::components::sapling::zip212_enforcement;
use zcash_primitives::transaction::fees::transparent::{InputSize, OutputView};
use zcash_primitives::transaction::fees::FeeRule;
use zcash_primitives::transaction::sighash::{PrecomputedSighashData, SignableInput};
use zcash_primitives::transaction::{Authorization, Authorized, Transaction, TransactionData, TxVersion, Unauthorized};
use zcash_protocol::consensus::{BlockHeight, BranchId, Parameters};
use zcash_protocol::memo::MemoBytes;
use zcash_protocol::value::{ZatBalance, Zatoshis};
use zcash_script::script::{Code as ScriptCode, FromChain as RedeemScript, Sig as ScriptSigData};
use zcash_transparent::address::{Script, TransparentAddress};
use zcash_transparent::builder::TransparentBuilder;
use zcash_transparent::bundle::{Authorized as TransparentAuthorized, Bundle as TransparentBundle, OutPoint, TxIn,
                                TxOut};
use zcash_transparent::sighash::{SighashType, SignableInput as TransparentSignableInput};

/// The expiry delta of every transaction this assembler builds, in blocks: the wallet's single
/// explicit value (40, the one the published builder's default had for these chains), not the library's
/// per-branch default, which moves to 120 blocks at NU7 (R39.11.8(e)).
pub(crate) use super::z_lightwalletd::BUILDER_EXPIRY_DELTA as Z_TX_EXPIRY_DELTA;

/// The `SIGHASH_ALL` byte appended to every transparent signature.
const SIGHASH_ALL_BYTE: u8 = 0x01;

/// The largest DER signature plus hash-type byte, used for the ZIP 317 size of a spending input
/// (the same bound the published transparent builder uses).
const MAX_SIGNATURE_WITH_HASH_TYPE: usize = 72 + 1;

/// The consensus limit on one script push, and so on a redeem script.
const MAX_REDEEM_SCRIPT_SIZE: usize = 520;

/// Size of an outpoint, and of a sequence number, in a serialized input.
const PREVOUT_SIZE: usize = 32 + 4;
const SEQUENCE_SIZE: usize = 4;

/// Why an assembly failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssemblerError {
    /// The transparent input or output data is not acceptable (a redeem script that does not match
    /// the spent output, script data that is not push-only, a payload that does not fit).
    Transparent(String),
    /// The Sapling part could not be built or proven.
    Sapling(String),
    /// The Ironwood part could not be built, or the transaction version has no Ironwood bundle.
    Ironwood(String),
    /// The caller's signer refused or failed.
    Signing(String),
    /// The fee rule could not price the transaction.
    Fee(String),
    /// The inputs fall short of outputs plus fee by this many zatoshis.
    InsufficientFunds(i64),
    /// The inputs exceed outputs plus fee by this many zatoshis, and no change output was added.
    ChangeRequired(i64),
    /// A value computation overflowed.
    Balance,
    /// The transaction could not be serialized into its final form.
    Freeze(String),
}

impl fmt::Display for AssemblerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AssemblerError::Transparent(e) => write!(f, "transparent part of the transaction: {e}"),
            AssemblerError::Sapling(e) => write!(f, "Sapling part of the transaction: {e}"),
            AssemblerError::Ironwood(e) => write!(f, "Ironwood part of the transaction: {e}"),
            AssemblerError::Signing(e) => write!(f, "signing: {e}"),
            AssemblerError::Fee(e) => write!(f, "fee: {e}"),
            AssemblerError::InsufficientFunds(short) => {
                write!(f, "insufficient funds: {short} zatoshis short of outputs plus fee")
            },
            AssemblerError::ChangeRequired(extra) => {
                write!(
                    f,
                    "a change output is required: {extra} zatoshis exceed outputs plus fee"
                )
            },
            AssemblerError::Balance => write!(f, "a value computation overflowed"),
            AssemblerError::Freeze(e) => write!(f, "finishing the transaction: {e}"),
        }
    }
}

impl std::error::Error for AssemblerError {}

/// A transparent signature as the signer returns it: the DER-encoded ECDSA signature without the
/// hash-type byte (the assembler appends `SIGHASH_ALL`), and the public key of the signing key.
pub struct TransparentSignature {
    pub der: Vec<u8>,
    pub public_key: Vec<u8>,
}

/// Signs a transparent signature hash. The assembler accepts the signature only when the signing key's
/// public half occurs in the redeem script it spends, so a key that is not the contract's cannot sign.
pub type TransparentSigner = Box<dyn Fn(&[u8; 32]) -> Result<TransparentSignature, String> + Send>;

/// A transparent input spending an atomic-swap P2SH output of this project's contract.
pub struct KdfP2shInput {
    /// The output being spent.
    pub outpoint: OutPoint,
    /// The previous output (its value and its P2SH script).
    pub coin: TxOut,
    /// The redeem script whose hash the previous output commits to; also the script code the
    /// signature hash commits to.
    pub redeem_script: Vec<u8>,
    /// The pushes between the signature and the redeem script: the claim secret, or the selector of
    /// the refund branch. Raw script bytes, push-only.
    pub script_data: Vec<u8>,
    /// The sequence number of the input (the refund branch needs a non-final one).
    pub sequence: u32,
    /// Signs the signature hash with the swap key.
    pub signer: TransparentSigner,
}

struct SaplingSpend {
    fvk: FullViewingKey,
    note: Note,
    path: MerklePath,
}

struct SaplingOutput {
    ovk: Option<OutgoingViewingKey>,
    to: PaymentAddress,
    value: Zatoshis,
    memo: MemoBytes,
}

struct IronwoodOutput {
    to: orchard::Address,
    value: Zatoshis,
    memo: MemoBytes,
}

/// The assembler: collect the parts, then [`TxAssembler::build`].
pub struct TxAssembler<P> {
    params: P,
    target_height: BlockHeight,
    branch: BranchId,
    version: TxVersion,
    expiry_height: BlockHeight,
    lock_time: u32,
    sapling_anchor: sapling::Anchor,
    sapling_spends: Vec<SaplingSpend>,
    sapling_outputs: Vec<SaplingOutput>,
    ironwood_outputs: Vec<IronwoodOutput>,
    p2sh_inputs: Vec<KdfP2shInput>,
    transparent_outputs: Vec<TxOut>,
}

/// The transaction version and consensus branch the assembler selects at `height`: the same rule the
/// published builder follows, so a coin without an Ironwood height keeps building version 4.
pub(crate) fn version_and_branch_at<P: Parameters>(params: &P, height: BlockHeight) -> (TxVersion, BranchId) {
    let branch = BranchId::for_height(params, height);
    (TxVersion::suggested_for_branch(branch), branch)
}

impl<P: Parameters> TxAssembler<P> {
    /// A new assembler for a transaction targeting `target_height`. The expiry height is the target
    /// height plus [`Z_TX_EXPIRY_DELTA`]; the lock time is zero until [`Self::set_lock_time`].
    pub fn new(params: P, target_height: BlockHeight, sapling_anchor: sapling::Anchor) -> Self {
        let (version, branch) = version_and_branch_at(&params, target_height);
        TxAssembler {
            params,
            target_height,
            branch,
            version,
            expiry_height: target_height + Z_TX_EXPIRY_DELTA,
            lock_time: 0,
            sapling_anchor,
            sapling_spends: Vec::new(),
            sapling_outputs: Vec::new(),
            ironwood_outputs: Vec::new(),
            p2sh_inputs: Vec::new(),
            transparent_outputs: Vec::new(),
        }
    }

    /// Sets the legacy `nLockTime` of the transaction.
    pub fn set_lock_time(&mut self, lock_time: u32) { self.lock_time = lock_time; }

    /// Adds a Sapling spend of `note`, witnessed by `path` under the anchor given at construction.
    pub fn add_sapling_spend(&mut self, fvk: FullViewingKey, note: Note, path: MerklePath) {
        self.sapling_spends.push(SaplingSpend { fvk, note, path });
    }

    /// Adds a Sapling output.
    pub fn add_sapling_output(
        &mut self,
        ovk: Option<OutgoingViewingKey>,
        to: PaymentAddress,
        value: Zatoshis,
        memo: MemoBytes,
    ) {
        self.sapling_outputs.push(SaplingOutput { ovk, to, value, memo });
    }

    /// Adds an output of the output-only Ironwood bundle. The bundle has spends disabled, is anchored
    /// at the empty-tree root and is padded to the default minimum of two actions (R39.6.5d).
    pub fn add_ironwood_output(&mut self, to: orchard::Address, value: Zatoshis, memo: MemoBytes) {
        self.ironwood_outputs.push(IronwoodOutput { to, value, memo });
    }

    /// Adds a transparent input spending a swap contract output.
    pub fn add_kdf_p2sh_input(&mut self, input: KdfP2shInput) { self.p2sh_inputs.push(input); }

    /// Adds a transparent output exactly as given; outputs keep the order they were added in.
    pub fn add_transparent_output_raw(&mut self, output: TxOut) { self.transparent_outputs.push(output); }

    /// Builds, proves, signs and finishes the transaction.
    ///
    /// `sapling_extsks` are the spending keys of the Sapling spends. The fee rule is asked for the
    /// fee of exactly the parts added, and the transaction must balance to it.
    #[allow(clippy::too_many_arguments)]
    pub fn build<R, SP, OP, FR>(
        self,
        mut rng: R,
        sapling_extsks: &[ExtendedSpendingKey],
        spend_prover: &SP,
        output_prover: &OP,
        fee_rule: &FR,
    ) -> Result<Transaction, AssemblerError>
    where
        R: Rng + CryptoRng,
        SP: SpendProver,
        OP: OutputProver,
        FR: FeeRule,
        FR::Error: fmt::Debug,
    {
        let TxAssembler {
            params,
            target_height,
            branch,
            version,
            expiry_height,
            lock_time,
            sapling_anchor,
            sapling_spends,
            sapling_outputs,
            ironwood_outputs,
            p2sh_inputs,
            transparent_outputs,
        } = self;

        if !ironwood_outputs.is_empty() && !version.has_ironwood() {
            return Err(AssemblerError::Ironwood(format!(
                "transaction version {version:?} (branch {branch:?}) has no Ironwood bundle"
            )));
        }

        // ---- Transparent part: the inputs through the published builder (it checks that each
        // redeem script hashes to the spent output), then the sequence numbers and the raw outputs
        // set on the public fields of the bundle.
        let mut input_specs = Vec::with_capacity(p2sh_inputs.len());
        let mut transparent_in_sat: i128 = 0;
        let mut input_sizes = Vec::with_capacity(p2sh_inputs.len());
        let mut transparent_builder = TransparentBuilder::empty();
        for input in p2sh_inputs {
            if input.redeem_script.len() > MAX_REDEEM_SCRIPT_SIZE {
                return Err(AssemblerError::Transparent(format!(
                    "the redeem script is {} bytes; the most a script push can carry is {MAX_REDEEM_SCRIPT_SIZE}",
                    input.redeem_script.len()
                )));
            }
            let redeem = RedeemScript::parse(&ScriptCode(input.redeem_script.clone()))
                .map_err(|e| AssemblerError::Transparent(format!("redeem script: {e:?}")))?;
            ScriptSigData::parse(&ScriptCode(input.script_data.clone()))
                .map_err(|e| AssemblerError::Transparent(format!("script data: {e:?}")))?;
            transparent_builder
                .add_p2sh_input(redeem, input.outpoint.clone(), input.coin.clone())
                .map_err(|e| AssemblerError::Transparent(format!("input: {e:?}")))?;
            transparent_in_sat += i128::from(u64::from(input.coin.value()));
            input_sizes.push(InputSize::Known(kdf_p2sh_input_size(
                input.redeem_script.len(),
                input.script_data.len(),
            )));
            input_specs.push(input);
        }
        let transparent_out_sat: i128 = transparent_outputs
            .iter()
            .map(|out| i128::from(u64::from(out.value())))
            .sum();
        let transparent_output_sizes: Vec<usize> =
            transparent_outputs.iter().map(OutputView::serialized_size).collect();
        if input_specs.is_empty() && !transparent_outputs.is_empty() {
            // The published builder only yields a bundle when it holds an input or an output; give
            // it a placeholder output that the raw outputs replace below.
            transparent_builder
                .add_output(&TransparentAddress::PublicKeyHash([0; 20]), Zatoshis::ZERO)
                .map_err(|e| AssemblerError::Transparent(format!("placeholder output: {e:?}")))?;
        }
        let mut unauthorized_transparent = transparent_builder.build();
        if let Some(bundle) = unauthorized_transparent.as_mut() {
            for (txin, spec) in bundle.vin.iter_mut().zip(input_specs.iter()) {
                *txin = TxIn::from_parts(txin.prevout().clone(), (), spec.sequence);
            }
            bundle.vout = transparent_outputs.clone();
        }

        // ---- Sapling part (before the fee: the builder knows the padded output count).
        let zip212 = zip212_enforcement(&params, target_height);
        let sapling_bundle_type = sapling_builder::BundleType::DEFAULT;
        let mut sapling_builder_opt = None;
        let mut sapling_vb: i128 = 0;
        let mut sapling_output_count = 0usize;
        if !sapling_spends.is_empty() || !sapling_outputs.is_empty() {
            let mut builder = sapling_builder::Builder::new(zip212, sapling_bundle_type, sapling_anchor);
            for spend in sapling_spends {
                builder
                    .add_spend(spend.fvk, spend.note, spend.path)
                    .map_err(|e| AssemblerError::Sapling(format!("spend: {e:?}")))?;
            }
            for out in sapling_outputs {
                builder
                    .add_output(
                        out.ovk,
                        out.to,
                        SaplingNoteValue::from_raw(u64::from(out.value)),
                        out.memo.into_bytes(),
                    )
                    .map_err(|e| AssemblerError::Sapling(format!("output: {e:?}")))?;
            }
            sapling_vb = i128::from(i64::from(builder.value_balance::<ZatBalance>()));
            sapling_output_count = sapling_bundle_type
                .num_outputs(builder.inputs().len(), builder.outputs().len())
                .map_err(|e| AssemblerError::Sapling(e.to_owned()))?;
            sapling_builder_opt = Some(builder);
        }
        let sapling_input_count = sapling_builder_opt.as_ref().map_or(0, |b| b.inputs().len());

        // ---- Ironwood part: output-only, spends disabled, empty-tree anchor, default padding.
        let ironwood_version = orchard::bundle::BundleVersion::ironwood_v3();
        let mut ironwood_builder_opt = None;
        let mut ironwood_vb: i128 = 0;
        let mut ironwood_action_count = 0usize;
        if !ironwood_outputs.is_empty() {
            let flags = orchard::bundle::Flags::SPENDS_DISABLED;
            let mut builder = orchard::builder::Builder::new(
                orchard::builder::BundleType::DEFAULT,
                ironwood_version,
                flags,
                orchard::Anchor::empty_tree(),
            )
            .map_err(|e| AssemblerError::Ironwood(format!("builder: {e:?}")))?;
            for out in ironwood_outputs {
                builder
                    .add_output(
                        None,
                        out.to,
                        orchard::value::NoteValue::from_raw(u64::from(out.value)),
                        out.memo.into_bytes(),
                    )
                    .map_err(|e| AssemblerError::Ironwood(format!("output: {e:?}")))?;
            }
            ironwood_vb = i128::from(i64::from(
                builder
                    .value_balance::<ZatBalance>()
                    .map_err(|_| AssemblerError::Balance)?,
            ));
            ironwood_action_count = builder
                .bundle_type()
                .num_actions(
                    flags,
                    builder.spends().len(),
                    builder.outputs().len() + builder.changes().len(),
                )
                .map_err(|e| AssemblerError::Ironwood(e.to_owned()))?;
            ironwood_builder_opt = Some(builder);
        }

        // ---- Fee and balance: the rule prices exactly these parts, and they must balance to it.
        let fee = fee_rule
            .fee_required(
                &params,
                target_height,
                input_sizes,
                transparent_output_sizes,
                sapling_input_count,
                sapling_output_count,
                0,
                ironwood_action_count,
            )
            .map_err(|e| AssemblerError::Fee(format!("{e:?}")))?;
        let balance_after_fee =
            transparent_in_sat - transparent_out_sat + sapling_vb + ironwood_vb - i128::from(u64::from(fee));
        let balance_after_fee = i64::try_from(balance_after_fee).map_err(|_| AssemblerError::Balance)?;
        if balance_after_fee < 0 {
            return Err(AssemblerError::InsufficientFunds(-balance_after_fee));
        }
        if balance_after_fee > 0 {
            return Err(AssemblerError::ChangeRequired(balance_after_fee));
        }

        // ---- Sapling bundle with proofs, Ironwood bundle unproven.
        let sapling_bundle = match sapling_builder_opt {
            Some(builder) => {
                let built = builder
                    .build::<SP, OP, _, ZatBalance>(sapling_extsks, &mut rng)
                    .map_err(|e| AssemblerError::Sapling(format!("build: {e:?}")))?;
                built.map(|(bundle, _meta)| bundle.create_proofs(spend_prover, output_prover, &mut rng, ()))
            },
            None => None,
        };
        let ironwood_bundle = match ironwood_builder_opt {
            Some(builder) => {
                let built = builder
                    .build::<ZatBalance>(&mut rng)
                    .map_err(|e| AssemblerError::Ironwood(format!("build: {e:?}")))?;
                built.map(|(bundle, _meta)| bundle)
            },
            None => None,
        };

        // ---- Signature hashes. The transaction data used for hashing holds clones of the bundles;
        // the originals are then proven and signed.
        let unauthorized: TransactionData<Unauthorized> = make_data(
            version,
            branch,
            lock_time,
            expiry_height,
            unauthorized_transparent.clone(),
            sapling_bundle.clone(),
            ironwood_bundle.clone(),
        )?;
        let sighash_data = PrecomputedSighashData::new(unauthorized);
        let shielded_sighash = *sighash_data.signature_hash(&SignableInput::Shielded).as_ref();

        let authorized_transparent = match unauthorized_transparent.as_ref() {
            Some(bundle) => {
                let mut vin = Vec::with_capacity(bundle.vin.len());
                for (index, spec) in input_specs.iter().enumerate() {
                    let script_code = Script(ScriptCode(spec.redeem_script.clone()));
                    let signable = TransparentSignableInput::from_parts(
                        bundle,
                        SighashType::ALL,
                        index,
                        &script_code,
                        spec.coin.script_pubkey(),
                        spec.coin.value(),
                    )
                    .map_err(|e| AssemblerError::Transparent(format!("signature input: {e:?}")))?;
                    let sighash = *sighash_data
                        .signature_hash(&SignableInput::Transparent(signable))
                        .as_ref();
                    let signed = (spec.signer)(&sighash).map_err(AssemblerError::Signing)?;
                    let key_is_in_contract = !signed.public_key.is_empty()
                        && spec
                            .redeem_script
                            .windows(signed.public_key.len())
                            .any(|window| window == signed.public_key.as_slice());
                    if !key_is_in_contract {
                        return Err(AssemblerError::Signing(
                            "the public half of the signing key is not in the redeem script".to_owned(),
                        ));
                    }
                    let mut signature = signed.der;
                    signature.push(SIGHASH_ALL_BYTE);
                    let mut script_sig = push_data(&signature)?;
                    script_sig.extend_from_slice(&spec.script_data);
                    script_sig.extend(push_data(&spec.redeem_script)?);
                    vin.push(TxIn::from_parts(
                        spec.outpoint.clone(),
                        Script(ScriptCode(script_sig)),
                        spec.sequence,
                    ));
                }
                Some(TransparentBundle {
                    vin,
                    vout: bundle.vout.clone(),
                    authorization: TransparentAuthorized,
                })
            },
            None => None,
        };

        let sapling_asks: Vec<_> = sapling_extsks.iter().map(|extsk| extsk.expsk().ask().clone()).collect();
        let authorized_sapling = match sapling_bundle {
            Some(bundle) => Some(
                bundle
                    .apply_signatures(&mut rng, shielded_sighash, &sapling_asks)
                    .map_err(|e| AssemblerError::Sapling(format!("signatures: {e:?}")))?,
            ),
            None => None,
        };
        let authorized_ironwood = match ironwood_bundle {
            Some(bundle) => {
                let proving_key = cached_orchard_proving_key(ironwood_version.circuit_version());
                let proven = bundle
                    .create_proof(proving_key, &mut rng)
                    .map_err(|e| AssemblerError::Ironwood(format!("proof: {e:?}")))?;
                // No spend authorizing keys: the bundle spends only dummy notes.
                Some(
                    proven
                        .apply_signatures(&mut rng, shielded_sighash, &[])
                        .map_err(|e| AssemblerError::Ironwood(format!("signatures: {e:?}")))?,
                )
            },
            None => None,
        };

        let authorized: TransactionData<Authorized> = make_data(
            version,
            branch,
            lock_time,
            expiry_height,
            authorized_transparent,
            authorized_sapling,
            authorized_ironwood,
        )?;
        authorized.freeze().map_err(|e| AssemblerError::Freeze(e.to_string()))
    }
}

/// Assembles the transaction data of the version at hand: versions 4 and 5 carry no Ironwood bundle,
/// version 6 carries the Orchard slot (empty here) and the Ironwood slot.
fn make_data<A: Authorization>(
    version: TxVersion,
    branch: BranchId,
    lock_time: u32,
    expiry_height: BlockHeight,
    transparent: Option<zcash_transparent::bundle::Bundle<A::TransparentAuth>>,
    sapling: Option<sapling::bundle::Bundle<A::SaplingAuth, ZatBalance>>,
    ironwood: Option<orchard::bundle::Bundle<A::OrchardAuth, ZatBalance>>,
) -> Result<TransactionData<A>, AssemblerError> {
    if version.has_ironwood() {
        Ok(TransactionData::from_parts_v6(
            branch,
            lock_time,
            expiry_height,
            transparent,
            sapling,
            None,
            ironwood,
        ))
    } else if ironwood.is_some() {
        Err(AssemblerError::Ironwood(format!(
            "transaction version {version:?} has no Ironwood bundle"
        )))
    } else {
        Ok(TransactionData::from_parts(
            version,
            branch,
            lock_time,
            expiry_height,
            transparent,
            None,
            sapling,
            None,
        ))
    }
}

/// A canonical (minimal) data push of `data`.
fn push_data(data: &[u8]) -> Result<Vec<u8>, AssemblerError> {
    let len = u16::try_from(data.len())
        .map_err(|_| AssemblerError::Transparent(format!("a push of {} bytes does not fit", data.len())))?;
    let mut out = Vec::with_capacity(data.len() + 3);
    match len {
        n if n < 0x4c => out.push(n as u8),
        n if n <= 0xff => {
            out.push(0x4c);
            out.push(n as u8);
        },
        n => {
            out.push(0x4d);
            out.extend_from_slice(&n.to_le_bytes());
        },
    }
    out.extend_from_slice(data);
    Ok(out)
}

/// The bytes a push of `len` bytes takes: the data and its length prefix.
fn push_len(len: usize) -> usize {
    len + match len {
        0..=0x4b => 1,
        0x4c..=0xff => 2,
        _ => 3,
    }
}

/// The CompactSize length prefix of `len`.
fn compact_size_len(len: usize) -> usize {
    match len {
        0..=252 => 1,
        253..=0xffff => 3,
        0x1_0000..=0xffff_ffff => 5,
        _ => 9,
    }
}

/// The ZIP 317 size of a swap-contract spending input: the outpoint, the CompactSize and bytes of the
/// script signature (a signature of the largest size, the script data, the pushed redeem script) and
/// the sequence number. Pass it to the fee rule as [`InputSize::Known`].
pub fn kdf_p2sh_input_size(redeem_script_len: usize, script_data_len: usize) -> usize {
    let script_sig_len = push_len(MAX_SIGNATURE_WITH_HASH_TYPE) + script_data_len + push_len(redeem_script_len);
    PREVOUT_SIZE + compact_size_len(script_sig_len) + script_sig_len + SEQUENCE_SIZE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::z_coin::z_v6_construction_tests::{params, NoProver};
    use crate::z_coin::{SAPLING_OUTPUT_NAME, SAPLING_SPEND_NAME};
    use incrementalmerkletree::{frontier::CommitmentTree, witness::IncrementalWitness};
    use sapling::Node;
    use zcash_primitives::transaction::fees::fixed::FeeRule as FixedFeeRule;
    use zcash_primitives::transaction::fees::zip317;
    use zcash_proofs::prover::LocalTxProver;
    use zcash_transparent::bundle::Authorization as TransparentAuthorization;
    use zcash_transparent::sighash::TransparentAuthorizingContext;

    const IRONWOOD_HEIGHT: u32 = 297;
    const TIME_LOCK: u32 = 1_793_235_835;

    #[test]
    fn a_push_is_minimal_for_every_length_class() {
        assert_eq!(push_data(&[7; 5]).unwrap()[..1], [5]);
        assert_eq!(push_data(&[7; 0x4b]).unwrap()[..1], [0x4b]);
        assert_eq!(push_data(&[7; 0x4c]).unwrap()[..2], [0x4c, 0x4c]);
        assert_eq!(push_data(&[7; 107]).unwrap()[..2], [0x4c, 107]);
        assert_eq!(push_data(&[7; 256]).unwrap()[..3], [0x4d, 0x00, 0x01]);
        for len in [0usize, 5, 0x4b, 0x4c, 0xff, 0x100, 520] {
            assert_eq!(push_len(len), push_data(&vec![7; len]).unwrap().len(), "length {len}");
        }
        // Never truncated: a push above 0xffff bytes is refused.
        assert!(matches!(
            push_data(&vec![7; 0x1_0000]),
            Err(AssemblerError::Transparent(_))
        ));
    }

    #[test]
    fn the_p2sh_input_size_counts_every_part_of_the_script_signature() {
        // outpoint 36 + CompactSize 1 + (1 + 73) signature push + 1 selector + (2 + 107) redeem push + sequence 4
        assert_eq!(kdf_p2sh_input_size(107, 1), 36 + 1 + 74 + 1 + 109 + 4);
    }

    #[test]
    fn compact_size_prefix_lengths() {
        assert_eq!(compact_size_len(0), 1);
        assert_eq!(compact_size_len(252), 1);
        assert_eq!(compact_size_len(253), 3);
        assert_eq!(compact_size_len(0x1_0000), 5);
    }

    // ---- helpers shared by the transaction tests ----------------------------------------------

    fn key(byte: u8) -> secp256k1::SecretKey { secp256k1::SecretKey::from_slice(&[byte; 32]).unwrap() }

    fn public_key(secret: &secp256k1::SecretKey) -> Vec<u8> {
        secp256k1::PublicKey::from_secret_key(&secp256k1::Secp256k1::signing_only(), secret)
            .serialize()
            .to_vec()
    }

    /// The redeem script of the swap contract's shape (`payment_script`): the refund branch checks the
    /// lock time and the first key, the claim branch checks the secret's size and hash and the second key.
    fn contract_script(pub_0: &[u8], pub_1: &[u8]) -> Vec<u8> {
        let mut script = vec![0x63, 0x04];
        script.extend(TIME_LOCK.to_le_bytes());
        script.extend([0xb1, 0x75, 33]);
        script.extend(pub_0);
        script.push(0xac);
        script.extend([0x67, 0x82, 0x01, 0x20, 0x88, 0xa9, 20]);
        script.extend([0xab; 20]);
        script.extend([0x88, 33]);
        script.extend(pub_1);
        script.extend([0xac, 0x68]);
        script
    }

    fn signer(secret: secp256k1::SecretKey) -> TransparentSigner {
        Box::new(move |sighash: &[u8; 32]| {
            let secp = secp256k1::Secp256k1::signing_only();
            let message = secp256k1::Message::from_digest(*sighash);
            Ok(TransparentSignature {
                der: secp.sign_ecdsa(&message, &secret).serialize_der().to_vec(),
                public_key: public_key(&secret),
            })
        })
    }

    fn p2sh_coin(redeem: &[u8], value: u64) -> TxOut {
        TxOut::new(
            Zatoshis::const_from_u64(value),
            TransparentAddress::ScriptHash(kdf_crypto::dhash160(redeem).take())
                .script()
                .into(),
        )
    }

    fn p2pkh_output(value: u64, tag: u8) -> TxOut {
        TxOut::new(
            Zatoshis::const_from_u64(value),
            TransparentAddress::PublicKeyHash([tag; 20]).script().into(),
        )
    }

    /// The pushes of a push-only script.
    fn pushes(script: &[u8]) -> Vec<Vec<u8>> {
        let mut i = 0;
        let mut out = vec![];
        while i < script.len() {
            let op = script[i];
            i += 1;
            let (len, advance) = match op {
                1..=0x4b => (usize::from(op), 0),
                0x4c => (usize::from(script[i]), 1),
                0x4d => (usize::from(u16::from_le_bytes([script[i], script[i + 1]])), 2),
                _ => {
                    out.push(vec![op]);
                    continue;
                },
            };
            i += advance;
            out.push(script[i..i + len].to_vec());
            i += len;
        }
        out
    }

    #[derive(Debug)]
    struct Context {
        amounts: Vec<Zatoshis>,
        script_pubkeys: Vec<Script>,
    }
    impl TransparentAuthorization for Context {
        type ScriptSig = ();
    }
    impl TransparentAuthorizingContext for Context {
        fn input_amounts(&self) -> Vec<Zatoshis> { self.amounts.clone() }

        fn input_scriptpubkeys(&self) -> Vec<Script> { self.script_pubkeys.clone() }
    }
    struct Verifying;
    impl Authorization for Verifying {
        type TransparentAuth = Context;
        type SaplingAuth = sapling::bundle::Authorized;
        type OrchardAuth = orchard::bundle::Authorized;
    }

    /// The signature hashes of a finished transaction, recomputed from its parsed bytes (the transparent
    /// part rebuilt with the spent coins), independent of the assembler's own computation: the shielded
    /// one and the transparent one of every input given the script code of each.
    fn recomputed_sighashes(tx: &Transaction, coins: &[TxOut], script_codes: &[Vec<u8>]) -> ([u8; 32], Vec<[u8; 32]>) {
        let bundle = tx.transparent_bundle().map(|b| TransparentBundle {
            vin: b
                .vin
                .iter()
                .map(|i| TxIn::<Context>::from_parts(i.prevout().clone(), (), i.sequence()))
                .collect(),
            vout: b.vout.clone(),
            authorization: Context {
                amounts: coins.iter().map(TxOut::value).collect(),
                script_pubkeys: coins.iter().map(|c| c.script_pubkey().clone()).collect(),
            },
        });
        let data: TransactionData<Verifying> = if tx.version().has_ironwood() {
            TransactionData::from_parts_v6(
                tx.consensus_branch_id(),
                tx.lock_time(),
                tx.expiry_height(),
                bundle,
                tx.sapling_bundle().cloned(),
                tx.orchard_bundle().cloned(),
                tx.ironwood_bundle().cloned(),
            )
        } else {
            TransactionData::from_parts(
                tx.version(),
                tx.consensus_branch_id(),
                tx.lock_time(),
                tx.expiry_height(),
                bundle,
                None,
                tx.sapling_bundle().cloned(),
                None,
            )
        };
        let sighash_data = PrecomputedSighashData::new(data);
        let shielded = *sighash_data.signature_hash(&SignableInput::Shielded).as_ref();
        let transparent_bundle = sighash_data.tx().transparent_bundle();
        let transparent = match transparent_bundle {
            Some(bundle) => script_codes
                .iter()
                .enumerate()
                .map(|(index, code)| {
                    let script_code = Script(ScriptCode(code.clone()));
                    let signable = TransparentSignableInput::from_parts(
                        bundle,
                        SighashType::ALL,
                        index,
                        &script_code,
                        coins[index].script_pubkey(),
                        coins[index].value(),
                    )
                    .unwrap();
                    *sighash_data
                        .signature_hash(&SignableInput::Transparent(signable))
                        .as_ref()
                })
                .collect(),
            None => vec![],
        };
        (shielded, transparent)
    }

    fn ecdsa_valid(der_with_type: &[u8], sighash: [u8; 32], public: &[u8]) -> bool {
        let (der, hash_type) = der_with_type.split_at(der_with_type.len() - 1);
        assert_eq!(hash_type, [SIGHASH_ALL_BYTE]);
        let signature = secp256k1::ecdsa::Signature::from_der(der).unwrap();
        let public = secp256k1::PublicKey::from_slice(public).unwrap();
        secp256k1::Secp256k1::verification_only()
            .verify_ecdsa(&secp256k1::Message::from_digest(sighash), &signature, &public)
            .is_ok()
    }

    struct Spend {
        data: Vec<u8>,
        sequence: u32,
        lock_time: u32,
    }

    /// Builds a transparent-only spend of the contract at `height`: one input, two raw outputs in a
    /// fixed order, a fixed fee. No Sapling part, so no prover and no randomness are involved.
    fn build_spend(
        height: u32,
        spend: &Spend,
        signing: secp256k1::SecretKey,
    ) -> Result<(Transaction, Vec<u8>, TxOut), AssemblerError> {
        let redeem = contract_script(&public_key(&key(0x11)), &public_key(&key(0x22)));
        let coin = p2sh_coin(&redeem, 100_000);
        let mut assembler = TxAssembler::new(
            params(Some(IRONWOOD_HEIGHT)),
            height.into(),
            sapling::Anchor::empty_tree(),
        );
        assembler.set_lock_time(spend.lock_time);
        assembler.add_kdf_p2sh_input(KdfP2shInput {
            outpoint: OutPoint::new([5; 32], 0),
            coin: coin.clone(),
            redeem_script: redeem.clone(),
            script_data: spend.data.clone(),
            sequence: spend.sequence,
            signer: signer(signing),
        });
        assembler.add_transparent_output_raw(p2pkh_output(60_000, 4));
        assembler.add_transparent_output_raw(p2pkh_output(39_000, 9));
        let tx = assembler.build(
            crate::z_coin::os_rng(),
            &[],
            &NoProver,
            &NoProver,
            &FixedFeeRule::non_standard(Zatoshis::const_from_u64(1_000)),
        )?;
        Ok((tx, redeem, coin))
    }

    // ---- transparent-only tests (need nothing) -------------------------------------------------

    /// The refund branch (selector `OP_1`, non-final sequence, lock time) and the claim branch (the secret
    /// pushed, final sequence, no lock time), as versions 4 and 6: the script signature is
    /// `<signature> <selector or secret> <redeem script>`, the signature is a valid ECDSA signature over
    /// the independently recomputed signature hash, and the outputs keep their order.
    #[test]
    fn the_refund_and_the_claim_carry_the_contract_script_signature() {
        let secret = [0x5a; 32];
        let claim_data = [&[0x20][..], &secret[..]].concat();
        let cases = [
            ("refund", Spend {
                data: vec![0x51],
                sequence: 0xffff_fffe,
                lock_time: TIME_LOCK,
            }),
            ("claim", Spend {
                data: claim_data,
                sequence: 0xffff_ffff,
                lock_time: 0,
            }),
        ];
        for (name, spend) in cases {
            for height in [IRONWOOD_HEIGHT - 1, IRONWOOD_HEIGHT + 10] {
                let (tx, redeem, coin) = build_spend(height, &spend, key(0x11)).unwrap();
                let label = format!("{name} at {height}");
                assert_eq!(tx.version().has_ironwood(), height >= IRONWOOD_HEIGHT, "{label}");
                assert_eq!(tx.lock_time(), spend.lock_time, "{label}");
                assert_eq!(u32::from(tx.expiry_height()), height + Z_TX_EXPIRY_DELTA, "{label}");

                // Survives the trip through the bytes the network sees.
                let mut bytes = Vec::new();
                tx.write(&mut bytes).unwrap();
                let read = Transaction::read(&bytes[..], tx.consensus_branch_id()).unwrap();

                let bundle = read.transparent_bundle().unwrap();
                assert_eq!(bundle.vin.len(), 1, "{label}");
                assert_eq!(bundle.vin[0].sequence(), spend.sequence, "{label}");
                assert_eq!(bundle.vin[0].prevout().hash(), &[5; 32], "{label}");
                let outputs: Vec<(u64, Vec<u8>)> = bundle
                    .vout
                    .iter()
                    .map(|o| (u64::from(o.value()), o.script_pubkey().0 .0.clone()))
                    .collect();
                assert_eq!(
                    outputs,
                    vec![
                        (60_000, p2pkh_output(60_000, 4).script_pubkey().0 .0.clone()),
                        (39_000, p2pkh_output(39_000, 9).script_pubkey().0 .0.clone()),
                    ],
                    "{label}: outputs keep their order"
                );

                let parts = pushes(&bundle.vin[0].script_sig().0 .0);
                assert_eq!(parts.len(), 3, "{label}: signature, selector or secret, redeem script");
                assert_eq!(parts[2], redeem, "{label}: the redeem script is the last push");
                let expected_data = pushes(&spend.data);
                assert_eq!(&parts[1..2], &expected_data[..], "{label}: the selector or secret");

                let (_, transparent) = recomputed_sighashes(&read, &[coin], &[redeem.clone()]);
                assert!(
                    ecdsa_valid(&parts[0], transparent[0], &public_key(&key(0x11))),
                    "{label}: the signature verifies against the recomputed signature hash"
                );
            }
        }
    }

    /// A key whose public half is not in the redeem script cannot sign (the signature would be
    /// worthless on the network); the failure is typed.
    #[test]
    fn a_key_that_is_not_the_contracts_cannot_sign() {
        let spend = Spend {
            data: vec![0x51],
            sequence: 0xffff_fffe,
            lock_time: TIME_LOCK,
        };
        let refused = build_spend(IRONWOOD_HEIGHT + 10, &spend, key(0x33)).unwrap_err();
        assert!(matches!(refused, AssemblerError::Signing(_)), "{refused}");
    }

    /// A redeem script above the 520-byte consensus limit of one push is refused with a typed error before
    /// anything is built.
    #[test]
    fn a_redeem_script_over_520_bytes_is_refused() {
        let redeem = vec![0x51u8; 521];
        let mut assembler = TxAssembler::new(
            params(Some(IRONWOOD_HEIGHT)),
            300u32.into(),
            sapling::Anchor::empty_tree(),
        );
        assembler.add_kdf_p2sh_input(KdfP2shInput {
            outpoint: OutPoint::new([5; 32], 0),
            coin: p2sh_coin(&redeem, 100_000),
            redeem_script: redeem,
            script_data: vec![0x51],
            sequence: 0xffff_fffe,
            signer: signer(key(0x11)),
        });
        assembler.add_transparent_output_raw(p2pkh_output(99_000, 4));
        let err = assembler
            .build(
                crate::z_coin::os_rng(),
                &[],
                &NoProver,
                &NoProver,
                &FixedFeeRule::non_standard(Zatoshis::const_from_u64(1_000)),
            )
            .unwrap_err();
        assert!(
            matches!(&err, AssemblerError::Transparent(m) if m.contains("521") && m.contains("520")),
            "{err}"
        );
    }

    /// The transaction must balance to the fee the rule names, and a shortfall or surplus is a typed error
    /// that says by how much.
    #[test]
    fn the_transaction_must_balance_to_the_fee() {
        let redeem = contract_script(&public_key(&key(0x11)), &public_key(&key(0x22)));
        for (outputs, expected) in [
            (vec![60_000u64, 40_000], AssemblerError::InsufficientFunds(1_000)),
            (vec![60_000, 30_000], AssemblerError::ChangeRequired(9_000)),
        ] {
            let mut assembler = TxAssembler::new(
                params(Some(IRONWOOD_HEIGHT)),
                300u32.into(),
                sapling::Anchor::empty_tree(),
            );
            assembler.add_kdf_p2sh_input(KdfP2shInput {
                outpoint: OutPoint::new([5; 32], 0),
                coin: p2sh_coin(&redeem, 100_000),
                redeem_script: redeem.clone(),
                script_data: vec![0x51],
                sequence: 0xffff_fffe,
                signer: signer(key(0x11)),
            });
            for value in outputs {
                assembler.add_transparent_output_raw(p2pkh_output(value, 4));
            }
            let err = assembler
                .build(
                    crate::z_coin::os_rng(),
                    &[],
                    &NoProver,
                    &NoProver,
                    &FixedFeeRule::non_standard(Zatoshis::const_from_u64(1_000)),
                )
                .unwrap_err();
            assert_eq!(err, expected);
        }
    }

    /// The ZIP 317 rule prices the contract-spending input through `InputSize::Known`: one input of the
    /// size this module computes, two transparent outputs, no shielded part. Two logical actions for the
    /// input (225 bytes over the 150-byte unit) and two for the outputs (66 bytes over the 34-byte unit)
    /// is the larger of the two, so the fee is 2 x 5000; the transaction then balances to it.
    #[test]
    fn the_zip317_rule_prices_the_contract_input() {
        let redeem = contract_script(&public_key(&key(0x11)), &public_key(&key(0x22)));
        let input_size = kdf_p2sh_input_size(redeem.len(), 1);
        assert_eq!(redeem.len(), 107);
        assert_eq!(input_size, 36 + 1 + 74 + 1 + 109 + 4);
        let fee = 10_000u64;
        let mut assembler = TxAssembler::new(
            params(Some(IRONWOOD_HEIGHT)),
            300u32.into(),
            sapling::Anchor::empty_tree(),
        );
        assembler.add_kdf_p2sh_input(KdfP2shInput {
            outpoint: OutPoint::new([5; 32], 0),
            coin: p2sh_coin(&redeem, 100_000),
            redeem_script: redeem.clone(),
            script_data: vec![0x51],
            sequence: 0xffff_fffe,
            signer: signer(key(0x11)),
        });
        assembler.add_transparent_output_raw(p2pkh_output(60_000, 4));
        assembler.add_transparent_output_raw(p2pkh_output(30_000, 9));
        let tx = assembler
            .build(
                crate::z_coin::os_rng(),
                &[],
                &NoProver,
                &NoProver,
                &zip317::FeeRule::standard(),
            )
            .expect("the standard fee of this shape is 10000");
        let paid = 100_000
            - tx.transparent_bundle()
                .unwrap()
                .vout
                .iter()
                .map(|o| u64::from(o.value()))
                .sum::<u64>();
        assert_eq!(paid, fee);
    }

    // ---- golden vectors from the vendored builder --------------------------------------------

    // The transactions below were produced by the builder this assembler replaces (the vendored
    // `zcash_primitives` 0.30.1 and `zcash_transparent` 0.10.0 of the stack before the library upgrade),
    // from exactly the inputs of `build_spend`: the keys, redeem script, outpoint, outputs and fee are
    // the same, the parameters have Sapling at 61 and Ironwood at 297, and the signatures are
    // deterministic. The refund at 296 and the claim at 296 are version 4; at 307 they are version 6.
    /// `refund` at height 296.
    const GOLDEN_REFUND_296: &str = "0400008085202f8901050505050505050505050505050505050505050505050505050505050505050500000000b7483045022100b7405e7b723a59d2b9fe9477c5e7bee3bb7bd5786d0c4a810366d54afad869b802204def1f199fa15136c46642bf12fed6aebc5fc9f2707e8c2bca28d14bff9d719801514c6b63047b9be26ab17521034f355bdcb7cc0af728ef3cceb9615d90684bb5b2ca5f859ab0f0b704075871aaac6782012088a914abababababababababababababababababababab882102466d7fcae563e5cb09a0d1870bb580344804617879a14949cf22285f1bae3f27ac68feffffff0260ea0000000000001976a914040404040404040404040404040404040404040488ac58980000000000001976a914090909090909090909090909090909090909090988ac7b9be26a500100000000000000000000000000";
    /// `refund` at height 307.
    const GOLDEN_REFUND_307: &str = "0600008098b684d85b16a5377b9be26a5b01000001050505050505050505050505050505050505050505050505050505050505050500000000b7483045022100c86e61c87da931f21c6e1aa749e7eca567ee7a8dcc10ddc19f2442d661e4c6a8022048e26eb5f20acd973228d845ca55243a083e7a3ba5bea73ef107afa23d66eae501514c6b63047b9be26ab17521034f355bdcb7cc0af728ef3cceb9615d90684bb5b2ca5f859ab0f0b704075871aaac6782012088a914abababababababababababababababababababab882102466d7fcae563e5cb09a0d1870bb580344804617879a14949cf22285f1bae3f27ac68feffffff0260ea0000000000001976a914040404040404040404040404040404040404040488ac58980000000000001976a914090909090909090909090909090909090909090988ac00000000";
    /// `claim` at height 296.
    const GOLDEN_CLAIM_296: &str = "0400008085202f8901050505050505050505050505050505050505050505050505050505050505050500000000d7483045022100fbe0a49aca8f2c580702431b03f1c8b24d77c8c34d1f9880f508aa3d218baa20022010d851bdfdbf2f9b35c28231c7cca050254a5826e1b7b6c92b21f4ec2de0cb2901205a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a4c6b63047b9be26ab17521034f355bdcb7cc0af728ef3cceb9615d90684bb5b2ca5f859ab0f0b704075871aaac6782012088a914abababababababababababababababababababab882102466d7fcae563e5cb09a0d1870bb580344804617879a14949cf22285f1bae3f27ac68ffffffff0260ea0000000000001976a914040404040404040404040404040404040404040488ac58980000000000001976a914090909090909090909090909090909090909090988ac00000000500100000000000000000000000000";
    /// `claim` at height 307.
    const GOLDEN_CLAIM_307: &str = "0600008098b684d85b16a537000000005b01000001050505050505050505050505050505050505050505050505050505050505050500000000d6473044022006623a3f90111317e9482875be5f57edf81e687977c075fcc22051923d9c0f6f0220345369fac8c2e50c948d8c7e6562eb20543edc2736c8a7c2f869a2e8ebf2b98001205a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a4c6b63047b9be26ab17521034f355bdcb7cc0af728ef3cceb9615d90684bb5b2ca5f859ab0f0b704075871aaac6782012088a914abababababababababababababababababababab882102466d7fcae563e5cb09a0d1870bb580344804617879a14949cf22285f1bae3f27ac68ffffffff0260ea0000000000001976a914040404040404040404040404040404040404040488ac58980000000000001976a914090909090909090909090909090909090909090988ac00000000";

    /// The assembler reproduces, byte for byte, what the vendored builder produced for the same refund
    /// and claim, as version 4 and as version 6.
    #[test]
    fn the_assembler_matches_the_vendored_builder_byte_for_byte() {
        let secret = [0x5a; 32];
        let claim_data = [&[0x20][..], &secret[..]].concat();
        let cases = [
            (
                296,
                "refund",
                Spend {
                    data: vec![0x51],
                    sequence: 0xffff_fffe,
                    lock_time: TIME_LOCK,
                },
                GOLDEN_REFUND_296,
            ),
            (
                307,
                "refund",
                Spend {
                    data: vec![0x51],
                    sequence: 0xffff_fffe,
                    lock_time: TIME_LOCK,
                },
                GOLDEN_REFUND_307,
            ),
            (
                296,
                "claim",
                Spend {
                    data: claim_data.clone(),
                    sequence: 0xffff_ffff,
                    lock_time: 0,
                },
                GOLDEN_CLAIM_296,
            ),
            (
                307,
                "claim",
                Spend {
                    data: claim_data,
                    sequence: 0xffff_ffff,
                    lock_time: 0,
                },
                GOLDEN_CLAIM_307,
            ),
        ];
        for (height, name, spend, golden) in cases {
            let (tx, _, _) = build_spend(height, &spend, key(0x11)).unwrap();
            let mut bytes = Vec::new();
            tx.write(&mut bytes).unwrap();
            assert_eq!(hex::encode(&bytes), golden, "{name} at {height}");
        }
    }

    // ---- tests that need the Sapling parameters -------------------------------------------------

    fn sapling_prover() -> Option<LocalTxProver> {
        let dir = std::env::var_os("ARRR_SAPLING_PARAMS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| crate::utxo::zcash_params_path().expect("default Zcash parameters directory"));
        let (spend, output) = (dir.join(SAPLING_SPEND_NAME), dir.join(SAPLING_OUTPUT_NAME));
        if spend.exists() && output.exists() {
            Some(LocalTxProver::new(&spend, &output))
        } else {
            eprintln!(
                "SKIPPED: Sapling parameters not found in {} (set ARRR_SAPLING_PARAMS)",
                dir.display()
            );
            None
        }
    }

    /// The fields the E2 probe recorded for the vendored builder and the assembler alike: a spend of the
    /// contract with a Sapling output next to two raw outputs.
    #[test]
    fn a_contract_spend_with_a_sapling_output_has_the_recorded_shape_and_verifies() {
        let Some(prover) = sapling_prover() else { return };
        let extsk = ExtendedSpendingKey::master(&[1u8; 32]).expect("a valid master key");
        let (_, address) = extsk.default_address();
        let redeem = contract_script(&public_key(&key(0x11)), &public_key(&key(0x22)));
        let coin = p2sh_coin(&redeem, 100_000);
        let mut assembler = TxAssembler::new(
            params(Some(IRONWOOD_HEIGHT)),
            1_000u32.into(),
            sapling::Anchor::empty_tree(),
        );
        assembler.set_lock_time(TIME_LOCK);
        assembler.add_kdf_p2sh_input(KdfP2shInput {
            outpoint: OutPoint::new([5; 32], 0),
            coin: coin.clone(),
            redeem_script: redeem.clone(),
            script_data: vec![0x51],
            sequence: 0xffff_fffe,
            signer: signer(key(0x11)),
        });
        assembler.add_transparent_output_raw(TxOut::new(
            Zatoshis::const_from_u64(10_000),
            TransparentAddress::ScriptHash([9; 20]).script().into(),
        ));
        assembler.add_transparent_output_raw(p2pkh_output(20_000, 4));
        assembler.add_sapling_output(None, address, Zatoshis::const_from_u64(69_000), MemoBytes::empty());
        let tx = assembler
            .build(
                crate::z_coin::os_rng(),
                &[],
                &prover,
                &prover,
                &FixedFeeRule::non_standard(Zatoshis::const_from_u64(1_000)),
            )
            .unwrap();

        assert_eq!(tx.version(), TxVersion::V6);
        assert_eq!(tx.consensus_branch_id(), BranchId::Nu6_3);
        assert_eq!(tx.lock_time(), TIME_LOCK);
        assert_eq!(u32::from(tx.expiry_height()), 1_040);
        let sapling = tx.sapling_bundle().unwrap();
        assert_eq!(
            (sapling.shielded_spends().len(), sapling.shielded_outputs().len()),
            (0, 2)
        );
        assert_eq!(i64::from(*sapling.value_balance()), -69_000);
        assert!(tx.orchard_bundle().is_none() && tx.ironwood_bundle().is_none());

        let (shielded, transparent) = recomputed_sighashes(&tx, &[coin], &[redeem]);
        let parts = pushes(&tx.transparent_bundle().unwrap().vin[0].script_sig().0 .0);
        assert!(ecdsa_valid(&parts[0], transparent[0], &public_key(&key(0x11))));
        let (spend_vk, output_vk) = prover.verifying_keys();
        let mut batch = sapling::BatchValidator::new();
        assert!(batch.check_bundle(sapling.clone(), shielded));
        assert!(batch.validate(&spend_vk, &output_vk, crate::z_coin::os_rng()));
    }

    /// The fields the E3 probe recorded: a Sapling spend whose value goes into an output-only Ironwood
    /// bundle (spends disabled, empty-tree anchor, two actions) and Sapling change; both proofs and all
    /// signatures verify under the public verifying keys. Needs the Sapling parameters and builds the
    /// Ironwood proving key (some seconds).
    #[test]
    fn a_sapling_to_ironwood_payment_has_the_recorded_output_only_bundle_and_verifies() {
        let Some(prover) = sapling_prover() else { return };
        let extsk = ExtendedSpendingKey::master(&[1u8; 32]).expect("a valid master key");
        let (_, address) = extsk.default_address();
        let note = Note::from_parts(
            address.clone(),
            SaplingNoteValue::from_raw(200_000),
            sapling::Rseed::AfterZip212([7; 32]),
        );
        let mut tree = CommitmentTree::<Node, 32>::empty();
        tree.append(Node::from_cmu(&note.cmu())).unwrap();
        let root = tree.root();
        let witness = IncrementalWitness::from_tree(tree).unwrap();
        let path = witness.path().unwrap();
        let ironwood_key = orchard::keys::SpendingKey::from_bytes([3; 32]).unwrap();
        let recipient =
            orchard::keys::FullViewingKey::from(&ironwood_key).address_at(0u32, orchard::keys::Scope::External);

        let mut assembler = TxAssembler::new(params(Some(IRONWOOD_HEIGHT)), 1_000u32.into(), root.into());
        let fvk = FullViewingKey::from_expanded_spending_key(extsk.expsk());
        assembler.add_sapling_spend(fvk, note, path);
        assembler.add_ironwood_output(recipient, Zatoshis::const_from_u64(150_000), MemoBytes::empty());
        assembler.add_sapling_output(None, address, Zatoshis::const_from_u64(49_000), MemoBytes::empty());
        let tx = assembler
            .build(
                crate::z_coin::os_rng(),
                std::slice::from_ref(&extsk),
                &prover,
                &prover,
                &FixedFeeRule::non_standard(Zatoshis::const_from_u64(1_000)),
            )
            .unwrap();

        assert_eq!(tx.version(), TxVersion::V6);
        assert_eq!(tx.lock_time(), 0);
        assert_eq!(u32::from(tx.expiry_height()), 1_040);
        assert!(tx.transparent_bundle().is_none() && tx.orchard_bundle().is_none());
        let sapling = tx.sapling_bundle().unwrap();
        assert_eq!(
            (sapling.shielded_spends().len(), sapling.shielded_outputs().len()),
            (1, 2)
        );
        assert_eq!(i64::from(*sapling.value_balance()), 151_000);
        let ironwood = tx.ironwood_bundle().unwrap();
        assert_eq!(ironwood.actions().len(), 2, "padded to the default two actions");
        assert_eq!(
            ironwood.flag_byte(),
            0x06,
            "spends disabled, outputs enabled, cross-address enabled"
        );
        assert_eq!(ironwood.anchor().to_bytes(), orchard::Anchor::empty_tree().to_bytes());
        assert_eq!(i64::from(*ironwood.value_balance()), -150_000);

        let (shielded, _) = recomputed_sighashes(&tx, &[], &[]);
        let (spend_vk, output_vk) = prover.verifying_keys();
        let mut sapling_batch = sapling::BatchValidator::new();
        assert!(sapling_batch.check_bundle(sapling.clone(), shielded));
        assert!(sapling_batch.validate(&spend_vk, &output_vk, crate::z_coin::os_rng()));
        let verifying_key = orchard::circuit::VerifyingKey::build(ironwood.bundle_version().circuit_version());
        let mut ironwood_batch = orchard::bundle::BatchValidator::new(&verifying_key);
        ironwood_batch
            .add_bundle(ironwood, shielded)
            .expect("the key supports the bundle");
        assert!(ironwood_batch.validate(crate::z_coin::os_rng()));
    }
}
