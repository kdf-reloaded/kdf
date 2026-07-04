#[allow(dead_code)]
const PROTOS: [&str; 5] = [
    "proto/messages.proto",
    "proto/messages-common.proto",
    "proto/messages-management.proto",
    "proto/messages-bitcoin.proto",
    "proto/messages-ethereum.proto",
];

fn main() {
    // prost_build::compile_protos(&PROTOS, &["proto"]).unwrap();
}
