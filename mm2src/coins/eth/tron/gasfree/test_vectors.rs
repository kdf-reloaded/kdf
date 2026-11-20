//! The test vectors of the GasFree group (CRD ch.63 T1, T2).
//!
//! Sources: the mainnet and Nile custody pairs, the Nile salts and init-code hashes, the Nile domain separator and
//! the five Nile messages with their hashes are cited data from the public GasFree SDK v1.1.2 (Apache-2.0); no
//! SDK code is used. The Shasta custody pairs are the answers of the live Shasta controller's
//! `getGasFreeAddress(address)` on 2026-11-19. The remaining pins (the mainnet and Shasta init-code hashes, domain
//! separators and message hashes) were computed independently of this code; the two message hashes were confirmed
//! on 2026-11-19 against the controllers' read-only `getMessageHashForTransfer`.

use super::super::{Network, TronAddress};

/// A fixture user: the same five on every network.
pub(super) struct UserVector {
    pub address: &'static str,
    /// The user's 20-byte address, left-padded to 32 bytes.
    pub salt: &'static str,
}

pub(super) const USERS: [UserVector; 5] = [
    UserVector {
        address: "TMVQGm1qAQYVdetCeGRRkTWYYrLXuHK2HC",
        salt: "0x0000000000000000000000007e5f4552091a69125d5dfcb7b8c2659029395bdf",
    },
    UserVector {
        address: "TDvSsdrNM5eeXNL3czpa6AxLDHZA9nwe9K",
        salt: "0x0000000000000000000000002b5ad5c4795c026514f8317c7a215e218dccd6cf",
    },
    UserVector {
        address: "TKTX96CBxr5kvhjsDHcqoiPWZageGxoTW3",
        salt: "0x0000000000000000000000006813eb9362372eef6200f3b1dbc3f819671cba69",
    },
    UserVector {
        address: "TCo75zcxTuWn5nnFqZUeK5socdVnG11f2T",
        salt: "0x0000000000000000000000001eff47bc3a10a45d4b230b5d10e37751fe6aa718",
    },
    UserVector {
        address: "TWYSVbUy6eTu6ZrFWRUimgDy9SinkggVKL",
        salt: "0x000000000000000000000000e1ab8145f7e55dc933d51a18c793f901a3a0b276",
    },
];

/// The custody address of one fixture user on one network, with the init-code hash of its derivation.
pub(super) struct CustodyVector {
    pub custody: &'static str,
    pub init_code_hash: &'static str,
}

const fn custody(custody: &'static str, init_code_hash: &'static str) -> CustodyVector {
    CustodyVector {
        custody,
        init_code_hash,
    }
}

/// Per network, the vectors of [`USERS`] in the same order.
pub(super) const CUSTODY_VECTORS: [(Network, [CustodyVector; 5]); 3] = [
    (Network::Mainnet, [
        custody(
            "TBwmA2PtMXC4HiGvfi8xg2jZd5i3y89DjK",
            "0x2eba1359b61b10ee7b99a56382df7cec47423cac9c7c9aa7191066a6571afeed",
        ),
        custody(
            "TTA7pGKZdpkJwiwuookcfbkdq6kZxysn86",
            "0x255723f52e235d682fe234f951b4dbf567bea05e19461d709aca3855871d850a",
        ),
        custody(
            "TJyjjMa8AKvARKyMRNm2JrZ6ftQh3cC6Zg",
            "0x0db662e4224038afd0c3c60a4ea7b714a0654421b3202938f1c857f5f42df164",
        ),
        custody(
            "TLCZMcXQuv7A9n1j3L8dTn2rr9oNe2Xqz3",
            "0x7e1e469e489ff3cdee79ddecd25f54c8968bfb4b311693614edb9b95b594be6a",
        ),
        custody(
            "TVKvPadRp1B5uagKTWi93YrwxfwPGEbbUQ",
            "0x52d53ead70923a719f463ad41be93cf2cab5b69a17d6c7303a16d4018d016d44",
        ),
    ]),
    (Network::Nile, [
        custody(
            "TUGC4eNuEgbaLotwxzzXEck1fRWru6n8ye",
            "0x05fc7930389726757627ebd6eccfa44c1ed3d61b7bc3eb2a52e33a10c72feb23",
        ),
        custody(
            "TLvVuqx74fMy8QMjEsMT4dWwmVbuNwYt8X",
            "0x4137db2fd612a7ece04620ebee511bc49ff1dd616f2fa754c337fc4cd3eca2f7",
        ),
        custody(
            "TTjqEjsitExzYsoDaR65nd3d2avhsXayfL",
            "0xab0d0d89b6d2921967b3ff3b13b0a9ece958a3a2b3e2c56a60f25e9fba01b722",
        ),
        custody(
            "TDd2QwVKX5ujRtCkWGNJUXRD16GyvYLZm3",
            "0x3b993129779481dcf8ca137aae0b3ad76e606333e8d1789538cdeafbfdf1dcf3",
        ),
        custody(
            "TUgxrm9ynUaPMSbDt65nXbWVD9fYcZyFKk",
            "0xf69dc6968343dcfb721a1275a476eb0e1c5e8e818865d58bcaecff55dda577f5",
        ),
    ]),
    (Network::Shasta, [
        custody(
            "TKSJukX9FWLAuiF5Uw75FKbmXVA7H6XaPU",
            "0xc4b1012e995babbb09ccedce40ca13b55a297efbafce425b8b247493c5d76cf5",
        ),
        custody(
            "TQbeSvDBVHEnZ1ggZs41xkHSgsXgA42Un7",
            "0x829442100b6e9f9490139304feb6ec656e31d5ad0f8bdf4a35ca59f8130c92eb",
        ),
        custody(
            "TPomj2Abnx3WuqKxhgJuXVGSGeuCd4jSeV",
            "0x89471451239e45a764972af1add7258e55dcf89ae1c0cfd137eacbf209dccc4e",
        ),
        custody(
            "TC1amaXXKqRuca4PcT46pmAC69RcLZZV9d",
            "0x79cee6ce9ee1524a7d594445c9e971ec699df57983a28906da595b59521c16f7",
        ),
        custody(
            "TKk6UY49DuzGKbWKhAJ4FkWSWW1ZbAysqM",
            "0x5d690c28ec958d417f7b606e6541e6b3ea79aa774a3b7b994e8bf78a8d0d4a8e",
        ),
    ]),
];

/// The Shasta custody addresses the public SDK v1.1.2 publishes for [`USERS`]: stale since the Shasta
/// controller's upgrade of 2026-07-04.
pub(super) const SHASTA_STALE_SDK_CUSTODY: [&str; 5] = [
    "TX5TyYzg9a15HksNfTkKbEqGxa7UEPuJE8",
    "TFPccGHtYiUBF5Gbke8Dnk6x1hUNLDA7Uw",
    "TFcDT19LqyVPkTenvV5yhtidkGxhEKxGmo",
    "TAt7QZUsP3d6JopDZLHpqcLRe1hUqPhAaa",
    "TR1Gq7N1w4W5qo8KefjGtZCDgPoTkYzoam",
];

/// The service provider of every message vector.
pub(super) const MESSAGE_SERVICE_PROVIDER: &str = "TDbJyQ6g1Lx9BAfEEeN5S5TMjjDRAVFCaA";
/// The fee cap of every message vector.
pub(super) const MESSAGE_MAX_FEE: u64 = 2000;

/// A `PermitTransfer` message vector with its Nile hashes.
pub(super) struct MessageVector {
    pub token: &'static str,
    pub user: &'static str,
    pub receiver: &'static str,
    pub value: u64,
    pub deadline: u64,
    pub nonce: u64,
    pub nile_struct_hash: &'static str,
    pub nile_full_hash: &'static str,
}

pub(super) const MESSAGES: [MessageVector; 5] = [
    MessageVector {
        token: "TXYZopYRdj2D9XRtbG411XZZ3kM5VkAeBf",
        user: "TMVQGm1qAQYVdetCeGRRkTWYYrLXuHK2HC",
        receiver: "TJM1BE5wq1VdHh3gwjUeyaVkvZp9DVYCfC",
        value: 10_000,
        deadline: 1_726_207_632,
        nonce: 2,
        nile_struct_hash: "0x66ddee4970f99745397d1da5037b1af25380b406e58d613e9a4e44c88f53f656",
        nile_full_hash: "0xb1226f3a0b690b04e2c39fac3b58352ed68943a12a54b58035045215aaf0b9b1",
    },
    MessageVector {
        token: "TLBaRhANQoJFTqre9Nf1mjuwNWjCJeYqUL",
        user: "TDvSsdrNM5eeXNL3czpa6AxLDHZA9nwe9K",
        receiver: "TLFXfejEMgivFDR2x8qBpukMXd56spmFhz",
        value: 20_000,
        deadline: 1_726_507_632,
        nonce: 3,
        nile_struct_hash: "0x810698dcc75464432adbb9ee4f3cabeb8340e59b278df4a67799cedcbd4ff2eb",
        nile_full_hash: "0x25c20423c18719438f4d40e6b8fec40ede6b73fb3fa702453ea9bd17dd154fb5",
    },
    MessageVector {
        token: "TVSvjZdyDSNocHm7dP3jvCmMNsCnMTPa5W",
        user: "TKTX96CBxr5kvhjsDHcqoiPWZageGxoTW3",
        receiver: "TX7WF4tRGQehC9W88XEEKBhQRkLmAtZqKo",
        value: 100_000,
        deadline: 1_729_507_632,
        nonce: 5,
        nile_struct_hash: "0x2904790f034a8932b4098421c9a471b340a3801aaaab2e59a70498bb87d680d3",
        nile_full_hash: "0x3d103a6a3407dfe7540696131d7cafc3d41d7d8649b93a95daeee041e66238ce",
    },
    MessageVector {
        token: "TWrZRHY9aKQZcyjpovdH6qeCEyYZrRQDZt",
        user: "TCo75zcxTuWn5nnFqZUeK5socdVnG11f2T",
        receiver: "TCN4biEVzzfyUgN1NM8iysp4bYx6mx2gPv",
        value: 100_000,
        deadline: 1_729_517_632,
        nonce: 15,
        nile_struct_hash: "0x3bf2b313fb43ef51fd25ef50e2ea48471879d1f9db00fcac9b1e7b054e27779d",
        nile_full_hash: "0xa1a612e946ad2fecc8bcd2f93f987c38a06ca4807db7af30442e9308a20234ea",
    },
    MessageVector {
        token: "TDnDyfMigx5nch7cCrtzGSwTXkUBnQJ9Pg",
        user: "TWYSVbUy6eTu6ZrFWRUimgDy9SinkggVKL",
        receiver: "TVkoisqxn1SbET8ztcnjqRGAY4npxqDcmv",
        value: 100_000,
        deadline: 1_729_907_632,
        nonce: 50,
        nile_struct_hash: "0xa2d2ed284f8300560812505b7699d3f9fde3bafacd5444ca3de812705ff2ebbf",
        nile_full_hash: "0xc78d11f0afc5397f9329861888a6724b66fe370f0189e67c39bfad4eeb7ec2a9",
    },
];

/// Per network: the domain separator and the full typed-data hash of the first message of [`MESSAGES`].
pub(super) const DOMAIN_VECTORS: [(Network, &str, &str); 3] = [
    (
        Network::Nile,
        "0x31a0a46f427dd040c91835228e4555951bde0a894cae6239869bb680ebc6ebea",
        "0xb1226f3a0b690b04e2c39fac3b58352ed68943a12a54b58035045215aaf0b9b1",
    ),
    (
        Network::Mainnet,
        "0x82f2b33881ada15cfdfa98b393db0e6f80fc9a27a4883ad62943ec5da825c9e8",
        "0xf8e96d82742565a5e4a661cda2b87344dbc69d0b766ecf96785f6d8c933c376e",
    ),
    (
        Network::Shasta,
        "0x9dca93e3eac3ce9330371ce98bef1401348ce5fe3366667fe91ad1810b69e382",
        "0x262789335eefe8158007d0b1178640a0a8733c5f1c7aec0b856ded23d3a83089",
    ),
];

/// The address a fixture gives in base58.
pub(super) fn address(base58: &str) -> TronAddress { TronAddress::from_base58(base58).expect("a fixture address") }

/// The 32 bytes a fixture gives in hex, with or without `0x`.
pub(super) fn hex32(text: &str) -> [u8; 32] {
    let bytes = hex::decode(text.strip_prefix("0x").unwrap_or(text)).expect("fixture hex");
    <[u8; 32]>::try_from(bytes.as_slice()).expect("32 fixture bytes")
}
