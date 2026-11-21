//! Regression vectors for the Solana SDK crate line (package S5): addresses, signatures, serialized transactions
//! and associated token addresses must stay byte-identical when the `solana-*` crates change their major version.
//! The expected values are literals taken from the output of the tree before the crate move.

use super::solana_types::generate_keypair_from_slice;
use super::spl::{associated_token_account, token_transfer_message, TokenTransferParams, SPL_TOKEN_PROGRAM_ID,
                 TOKEN_2022_PROGRAM_ID};
use super::*;
use base58::ToBase58;
use bincode::serialize;
use ed25519_dalek_bip32::{DerivationPath, ExtendedSigningKey};
use solana_hash::Hash;
use std::str::FromStr;

// Public test vectors: fixed seed bytes 1..=32 and the standard BIP-39 test mnemonic. They hold no funds and are
// not secrets. The expected values below were produced from the 2.x Solana crates before the move to 3.x.
const SINGLE_SEED: [u8; 32] = [
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
    32,
];
const MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
const HD_PATH: &str = "m/44'/501'/0'/0'";
const SIGNED_MESSAGE: &[u8] = b"solana sdk vector message";

fn single_keypair() -> Keypair { generate_keypair_from_slice(&SINGLE_SEED).expect("valid seed") }

fn hd_keypair() -> Keypair {
    let seed = bip39::Seed::new(
        &bip39::Mnemonic::from_phrase(MNEMONIC, bip39::Language::English).unwrap(),
        "",
    );
    let path = DerivationPath::from_str(HD_PATH).unwrap();
    let key = ExtendedSigningKey::from_seed(seed.as_bytes())
        .unwrap()
        .derive(&path)
        .unwrap();
    generate_keypair_from_slice(&key.signing_key.to_bytes()).expect("valid secret")
}

fn blockhash() -> Hash { Hash::new_from_array([7; 32]) }
fn recipient() -> Pubkey { Pubkey::new_from_array([9; 32]) }
fn mint() -> Pubkey { Pubkey::new_from_array([5; 32]) }

fn sol_transfer_hex() -> String {
    let tx = solana_system_transaction::transfer(&single_keypair(), &recipient(), 1_234_567, blockhash());
    hex::encode(serialize(&tx).unwrap())
}

fn spl_transfer_hex(create_destination: bool) -> String {
    let wallet = single_keypair();
    let wallet_key = wallet.pubkey();
    let recipient = recipient();
    let mint = mint();
    let params = TokenTransferParams {
        wallet: &wallet_key,
        recipient: &recipient,
        mint: &mint,
        program_id: &SPL_TOKEN_PROGRAM_ID,
        decimals: 6,
        amount: 42_000_000,
        create_destination,
    };
    let message = token_transfer_message(&params, &blockhash()).unwrap();
    let tx = Transaction::new(&[&wallet], message, blockhash());
    hex::encode(serialize(&tx).unwrap())
}

const SINGLE_KEYPAIR_B58: &str =
    "2Ana1pUpv2ZbMVkwF5FXapYeBEjdxDatLn7nvJkhgTSdZd8hbDHTd21as7EAsg7ypityqfsw2pMQKJcVDVcAEsd";
const SINGLE_ADDRESS: &str = "9C6hybhQ6Aycep9jaUnP6uL9ZYvDjUp1aSkFWPUFJtpj";
const HD_KEYPAIR_B58: &str = "27npWoNE4HfmLeQo1TyWcW7NEA28qnsnDK7kcttDQEWrCWnro83HMJ97rMmpvYYZRwDAvG4KRuB7hTBacvwD7bgi";
const HD_ADDRESS: &str = "HAgk14JpMQLgt6rVgv7cBQFJWFto5Dqxi472uT3DKpqk";
const SIGNATURE: &str = "4qAoGRHgyXZTSePDGrVWMTK32tzmVWfzTqU6rDtndydr2h5pqtVSFHoT4jrm5R4MQiWUvXC7Yno9PUwSbesQipTr";
const SOL_TX_HEX: &str = "01d8c38d1d0c6fa182d2439072b10296077f60cfbb2d5df66d06c526832069987f3380b2d0968d2b4a499b315c7319bfb8c4ce975474ebe4db9d5b0ee660e6d2050100010379b5562e8fe654f94078b112e8a98ba7901f853ae695bed7e0e3910bad04966409090909090909090909090909090909090909090909090909090909090909090000000000000000000000000000000000000000000000000000000000000000070707070707070707070707070707070707070707070707070707070707070701020200010c0200000087d6120000000000";
const SPL_TX_NO_CREATE_HEX: &str = "01075fe90521236e3fc2d3ac52325abe3462ea36b3f2ec36441d0dabf5c6a392ce57dfe427dfca25cc4b6173991b6f8b833ba5c0d3fdfff568b81de045ed22b70e0100020579b5562e8fe654f94078b112e8a98ba7901f853ae695bed7e0e3910bad0496643c509063f9b532e6e46e0a14a16f23de10276b79ae33a2a78a4158d9135560bf68d8faa1d52e0499884f4423516717bf606be35593840a43f4e22bb17e6b085b050505050505050505050505050505050505050505050505050505050505050506ddf6e1d765a193d9cbe146ceeb79ac1cb485ed5f5b37913a8cf5857eff00a90707070707070707070707070707070707070707070707070707070707070707010404010302000a0c80de80020000000006";
const SPL_TX_CREATE_HEX: &str = "0180fe2358e5901e7b26dbc2f21aedcc1b70f24bdb3ee815d8f00ad40b7410d20d50a803b680bbcf9cc683672a9247eecd22f03adebe0f2551cc12d77b278c68030100050879b5562e8fe654f94078b112e8a98ba7901f853ae695bed7e0e3910bad0496643c509063f9b532e6e46e0a14a16f23de10276b79ae33a2a78a4158d9135560bf68d8faa1d52e0499884f4423516717bf606be35593840a43f4e22bb17e6b085b0000000000000000000000000000000000000000000000000000000000000000050505050505050505050505050505050505050505050505050505050505050506ddf6e1d765a193d9cbe146ceeb79ac1cb485ed5f5b37913a8cf5857eff00a909090909090909090909090909090909090909090909090909090909090909098c97258f4e2489f1bb3d1029148e0d830b5a1399daff1084048e7bd8dbe9f859070707070707070707070707070707070707070707070707070707070707070702070600020604030501010504010402000a0c80de80020000000006";
const ATA_CLASSIC: &str = "54SiMs2PBJQDhh68u3DLVjXzS9DvKRgRm4qWvgVoAEq4";
const ATA_2022: &str = "5RPyFgd5LpfRyg3ErE8nv5zb7nbhNZczwKgy6upswhx5";

#[test]
fn single_key_keypair_and_address_are_unchanged() {
    let key_pair = single_keypair();
    assert!(
        key_pair.to_bytes()[..].to_base58() == SINGLE_KEYPAIR_B58,
        "single-key keypair bytes changed"
    );
    assert!(
        key_pair.pubkey().to_string() == SINGLE_ADDRESS,
        "single-key address changed"
    );
}

#[test]
fn hd_path_keypair_and_address_are_unchanged() {
    let key_pair = hd_keypair();
    assert!(
        key_pair.to_bytes()[..].to_base58() == HD_KEYPAIR_B58,
        "HD keypair bytes changed"
    );
    assert!(key_pair.pubkey().to_string() == HD_ADDRESS, "HD address changed");
}

#[test]
fn signature_over_a_fixed_message_is_unchanged() {
    let key_pair = single_keypair();
    let signature = key_pair.try_sign_message(SIGNED_MESSAGE).expect("signing succeeds");
    assert!(signature.to_string() == SIGNATURE, "signature bytes changed");
    assert!(
        signature.verify(key_pair.pubkey().as_ref(), SIGNED_MESSAGE),
        "signature does not verify"
    );
}

#[test]
fn sol_transfer_serialization_is_unchanged() {
    assert!(sol_transfer_hex() == SOL_TX_HEX, "serialized SOL transfer changed");
}

#[test]
fn spl_transfer_serialization_is_unchanged() {
    assert!(
        spl_transfer_hex(false) == SPL_TX_NO_CREATE_HEX,
        "serialized SPL transfer changed"
    );
    assert!(
        spl_transfer_hex(true) == SPL_TX_CREATE_HEX,
        "serialized SPL transfer with account creation changed"
    );
}

#[test]
fn associated_token_addresses_are_unchanged() {
    let wallet = single_keypair().pubkey();
    let classic = associated_token_account(&wallet, &mint(), &SPL_TOKEN_PROGRAM_ID);
    let token_2022 = associated_token_account(&wallet, &mint(), &TOKEN_2022_PROGRAM_ID);
    assert!(
        classic.to_string() == ATA_CLASSIC,
        "classic associated token address changed"
    );
    assert!(
        token_2022.to_string() == ATA_2022,
        "Token-2022 associated token address changed"
    );
}
