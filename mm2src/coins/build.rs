fn main() {
    let mut prost = prost_build::Config::new();
    prost.out_dir("utxo");
    prost.compile_protos(&["utxo/bchrpc.proto"], &["utxo"]).unwrap();

    // The generated lightwalletd messages must implement prost 0.14's `Message` (that is what
    // tonic 0.14 and `zcash_client_backend` use), while this crate's `prost` name stays on 0.12
    // for the other protobuf users. `prost_14` is the renamed 0.14 dependency.
    let mut config = prost_build_14::Config::new();
    config.prost_path("::prost_14");
    tonic_prost_build::configure()
        .build_server(false)
        .compile_with_config(config, &["z_coin/service.proto"], &["z_coin"])
        .unwrap();
}
