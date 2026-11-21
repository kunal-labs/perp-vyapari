fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Use a vendored protoc so builds need no system protobuf install.
    std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);
    let proto_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../proto");
    let proto = format!("{proto_dir}/venue.proto");
    println!("cargo:rerun-if-changed={proto}");
    tonic_prost_build::configure().compile_protos(&[proto.as_str()], &[proto_dir])?;
    Ok(())
}
