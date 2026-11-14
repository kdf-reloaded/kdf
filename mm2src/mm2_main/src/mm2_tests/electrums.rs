use serde_json::Value as Json;

// DOC/MARTY are the live successors of the retired RICK/MORTY dev assetchains
// (RICK/MORTY were removed from the GLEECBTC/coins registry entirely). Per that
// registry the canonical servers use coin-PREFIXED hostnames, not the bare
// `electrumN.cipig.net` hosts the old RICK/MORTY tests relied on:
//   DOC   -> doc.electrumN.cipig.net   :10020 (TCP) / :30020 (WSS) / :20020 (SSL)
//   MARTY -> marty.electrumN.cipig.net :10021 (TCP) / :30021 (WSS) / :20021 (SSL)
// All three hosts (N = 1,2,3) are listed; the client fails over across them, so
// the temporary electrum3 outage (hardware fault, fix planned) does not break
// tests as long as electrum1/electrum2 answer.
#[allow(dead_code)]
#[cfg(target_arch = "wasm32")]
pub fn doc_electrums() -> Vec<Json> {
    vec![
        json!({ "url": "doc.electrum1.cipig.net:30020", "protocol": "WSS" }),
        json!({ "url": "doc.electrum2.cipig.net:30020", "protocol": "WSS" }),
        json!({ "url": "doc.electrum3.cipig.net:30020", "protocol": "WSS" }),
    ]
}

#[allow(dead_code)]
#[cfg(not(target_arch = "wasm32"))]
pub fn doc_electrums() -> Vec<Json> {
    vec![
        json!({ "url": "doc.electrum1.cipig.net:10020" }),
        json!({ "url": "doc.electrum2.cipig.net:10020" }),
        json!({ "url": "doc.electrum3.cipig.net:10020" }),
    ]
}

#[allow(dead_code)]
#[cfg(target_arch = "wasm32")]
pub fn marty_electrums() -> Vec<Json> {
    vec![
        json!({ "url": "marty.electrum1.cipig.net:30021", "protocol": "WSS" }),
        json!({ "url": "marty.electrum2.cipig.net:30021", "protocol": "WSS" }),
        json!({ "url": "marty.electrum3.cipig.net:30021", "protocol": "WSS" }),
    ]
}

#[allow(dead_code)]
#[cfg(not(target_arch = "wasm32"))]
pub fn marty_electrums() -> Vec<Json> {
    vec![
        json!({ "url": "marty.electrum1.cipig.net:10021" }),
        json!({ "url": "marty.electrum2.cipig.net:10021" }),
        json!({ "url": "marty.electrum3.cipig.net:10021" }),
    ]
}

#[allow(dead_code)]
#[cfg(target_arch = "wasm32")]
pub fn tbtc_electrums() -> Vec<Json> {
    vec![
        json!({ "url": "electrum1.cipig.net:30068", "protocol": "WSS" }),
        json!({ "url": "electrum2.cipig.net:30068", "protocol": "WSS" }),
        json!({ "url": "electrum3.cipig.net:30068", "protocol": "WSS" }),
    ]
}

#[allow(dead_code)]
#[cfg(not(target_arch = "wasm32"))]
pub fn tbtc_electrums() -> Vec<Json> {
    vec![
        json!({ "url": "blockstream.info:143" }),
        json!({ "url": "blackie.c3-soft.com:57005" }),
        json!({ "url": "testnet.qtornado.com:51001" }),
    ]
}

#[cfg(target_arch = "wasm32")]
pub fn qtum_electrums() -> Vec<Json> {
    vec![
        json!({ "url": "electrum1.cipig.net:30071", "protocol": "WSS" }),
        json!({ "url": "electrum2.cipig.net:30071", "protocol": "WSS" }),
        json!({ "url": "electrum3.cipig.net:30071", "protocol": "WSS" }),
    ]
}

// The server list of the tQTUM and QRC20-on-testnet tests (native builds).
//
// There is exactly one public Qtum TESTNET Electrum server, `s6.electrum.qtum.site`, so this is the only entry:
// - The `s1`, `s3` and `s4` hosts of `qtum.info` on port 50001 serve Qtum MAINNET, so a testnet test pointed at
//   them reads the wrong chain; they are not listed.
// - Port 51002 of the same host is TLS, but its certificate is X.509 version 1, which this project refuses in
//   every mode (CRD ch.38 R38.6.4m item 5); it can never connect, so only plain TCP on port 51001 is listed.
// - Its history answers can be slow. A request gets a fixed 20 s once the connection is up (R38.6.4e item 2);
//   the server entry's `timeout_sec` bounds only establishing the connection (R38.6.4e item 1). Run a test that
//   failed on a slow answer again before suspecting the code.
//
// Add a second testnet server here if one appears.
#[cfg(not(target_arch = "wasm32"))]
pub const TQTUM_TEST_SERVERS: &[&str] = &["s6.electrum.qtum.site:51001"];

#[cfg(not(target_arch = "wasm32"))]
pub fn qtum_electrums() -> Vec<Json> { TQTUM_TEST_SERVERS.iter().map(|url| json!({ "url": url })).collect() }
