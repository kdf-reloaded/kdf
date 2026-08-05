use super::*;
use crate::solana::spl::{SplToken, SplTokenConf};
use bip39::Language;
use crypto::privkey::key_pair_from_seed;
use ed25519_dalek_bip32::{DerivationPath, ExtendedSigningKey};
use mm2_core::mm_ctx::MmCtxBuilder;
use std::str::FromStr;

pub enum SolanaNet {
    //Mainnet,
    Testnet,
    Devnet,
}

pub fn solana_net_to_url(net_type: SolanaNet) -> String {
    match net_type {
        //SolanaNet::Mainnet => "https://api.mainnet-beta.solana.com".to_string(),
        SolanaNet::Testnet => "https://api.testnet.solana.com/".to_string(),
        SolanaNet::Devnet => "https://api.devnet.solana.com".to_string(),
    }
}

pub fn generate_key_pair_from_seed(seed: String) -> Keypair {
    let derivation_path = DerivationPath::from_str("m/44'/501'/0'").unwrap();
    let mnemonic = bip39::Mnemonic::from_phrase(seed.as_str(), Language::English).unwrap();
    let seed = bip39::Seed::new(&mnemonic, "");
    let seed_bytes: &[u8] = seed.as_bytes();

    let ext = ExtendedSigningKey::from_seed(seed_bytes)
        .unwrap()
        .derive(&derivation_path)
        .unwrap();

    solana_keypair::keypair_from_seed(ext.signing_key.to_keypair_bytes().as_ref()).unwrap()
}

pub fn generate_key_pair_from_iguana_seed(seed: String) -> Keypair {
    let key_pair = key_pair_from_seed(seed.as_str()).unwrap();
    let secret: [u8; 32] = key_pair.private().secret.as_slice().try_into().unwrap();
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&secret);
    solana_keypair::keypair_from_seed(signing_key.to_keypair_bytes().as_ref()).unwrap()
}

pub fn spl_coin_for_test(
    solana_coin: SolanaCoin,
    ticker: String,
    decimals: u8,
    token_contract_address: Pubkey,
) -> SplToken {
    let spl_coin = SplToken {
        conf: Arc::new(SplTokenConf {
            decimals,
            ticker,
            token_contract_address,
        }),
        platform_coin: solana_coin,
    };
    spl_coin
}

pub fn solana_coin_for_test(seed: String, net_type: SolanaNet) -> (MmArc, SolanaCoin) {
    let url = solana_net_to_url(net_type);
    let client = SolanaRpcPool::with_commitment(vec![url], CommitmentConfig {
        commitment: CommitmentLevel::Finalized,
    })
    .expect("non-empty endpoint list");
    let conf = json!({
        "coins":[
           {"coin":"SOL","name":"solana","protocol":{"type":"SOL"},"rpcport":80,"mm2":1}
        ]
    });
    let ctx = MmCtxBuilder::new().with_conf(conf.clone()).into_mm_arc();
    let (ticker, decimals) = ("SOL".to_string(), 8);
    let key_pair = generate_key_pair_from_iguana_seed(seed);
    let my_address = key_pair.pubkey().to_string();
    let spl_tokens_infos = Arc::new(Mutex::new(HashMap::new()));

    let solana_coin = SolanaCoin(Arc::new(SolanaCoinImpl {
        decimals,
        my_address,
        key_pair,
        ticker,
        client,
        spl_tokens_infos,
    }));
    (ctx, solana_coin)
}
