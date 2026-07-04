use serde_json::Value as Json;

#[allow(dead_code)]
#[cfg(target_arch = "wasm32")]
pub fn rick_electrums() -> Vec<Json> {
    vec![
        json!({ "url": "electrum1.cipig.net:30017", "protocol": "WSS" }),
        json!({ "url": "electrum2.cipig.net:30017", "protocol": "WSS" }),
        json!({ "url": "electrum3.cipig.net:30017", "protocol": "WSS" }),
    ]
}

#[allow(dead_code)]
#[cfg(not(target_arch = "wasm32"))]
pub fn rick_electrums() -> Vec<Json> {
    vec![
        json!({ "url": "electrum1.cipig.net:10017" }),
        json!({ "url": "electrum2.cipig.net:10017" }),
        json!({ "url": "electrum3.cipig.net:10017" }),
    ]
}

#[allow(dead_code)]
#[cfg(target_arch = "wasm32")]
pub fn morty_electrums() -> Vec<Json> {
    vec![
        json!({ "url": "electrum1.cipig.net:30018", "protocol": "WSS" }),
        json!({ "url": "electrum2.cipig.net:30018", "protocol": "WSS" }),
        json!({ "url": "electrum3.cipig.net:30018", "protocol": "WSS" }),
    ]
}

#[allow(dead_code)]
#[cfg(not(target_arch = "wasm32"))]
pub fn morty_electrums() -> Vec<Json> {
    vec![
        json!({ "url": "electrum1.cipig.net:10018" }),
        json!({ "url": "electrum2.cipig.net:10018" }),
        json!({ "url": "electrum3.cipig.net:10018" }),
    ]
}

// DOC/MARTY are the live successors of the retired RICK/MORTY dev assetchains
// (RICK/MORTY were removed from the GLEECBTC/coins registry entirely). Per that
// registry the canonical servers use coin-PREFIXED hostnames, not the bare
// `electrumN.cipig.net` hosts the old RICK/MORTY tests relied on:
//   DOC   -> doc.electrumN.cipig.net   :10020 (TCP) / :30020 (WSS) / :20020 (SSL)
//   MARTY -> marty.electrumN.cipig.net :10021 (TCP) / :30021 (WSS) / :20021 (SSL)
// All three hosts (N = 1,2,3) are listed; the client fails over across them, so
// the temporary electrum3 outage (hardware fault, fix planned) does not break
// tests as long as electrum1/electrum2 answer. The RICK/MORTY helpers above are
// kept only so any not-yet-migrated tests stay compilable.
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

#[cfg(not(target_arch = "wasm32"))]
pub fn qtum_electrums() -> Vec<Json> {
    vec![
        json!({ "url": "s1.qtum.info:50001" }),
        json!({ "url": "s4.qtum.info:50001" }),
        json!({ "url": "s1.qtum.info:50001" }),
    ]
}
