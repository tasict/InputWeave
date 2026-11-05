//! Compiles `proto/` with protox, so no `protoc` binary is needed.

use std::path::PathBuf;

const PROTOS: &[&str] = &[
    "inputweave/peer/v1/peer.proto",
    "inputweave/app/v1/app.proto",
    "inputweave/service/v1/service.proto",
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../proto");
    let files: Vec<PathBuf> = PROTOS.iter().map(|p| root.join(p)).collect();
    for f in &files {
        println!("cargo:rerun-if-changed={}", f.display());
    }
    let descriptors = protox::compile(&files, [&root])?;
    prost_build::Config::new().compile_fds(descriptors)?;
    Ok(())
}
