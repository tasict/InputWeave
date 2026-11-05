//! Generates the Swift protobuf code the macOS app uses for IPC, without
//! `protoc`: protox compiles `proto/`, and the request is handed to
//! swift-protobuf's `protoc-gen-swift` plugin directly.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use prost::Message;
use prost_types::compiler::{CodeGeneratorRequest, CodeGeneratorResponse, Version};

/// The protos the app needs: only the IPC schema, not the peer protocol.
const PROTOS: &[&str] = &["inputweave/app/v1/app.proto"];
pub const OUTPUT: &str = "apps/macos/InputWeave/Generated";

pub fn generate(root: &Path, plugin: &Path) -> Result<Vec<String>, String> {
    let proto_root = root.join("proto");
    let files: Vec<_> = PROTOS.iter().map(|p| proto_root.join(p)).collect();
    let set = protox::compile(&files, [&proto_root]).map_err(|e| e.to_string())?;
    let request = CodeGeneratorRequest {
        file_to_generate: PROTOS.iter().map(|p| (*p).to_owned()).collect(),
        parameter: Some("Visibility=Internal,FileNaming=DropPath".to_owned()),
        proto_file: set.file,
        // The plugin only uses this to decide how much JSON support protoc
        // has; protox produces descriptors like a current protoc does.
        compiler_version: Some(Version {
            major: Some(3),
            minor: Some(21),
            patch: Some(0),
            suffix: Some("protox".to_owned()),
        }),
    };
    let mut child = Command::new(plugin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", plugin.display()))?;
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(&request.encode_to_vec())
        .map_err(|e| e.to_string())?;
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!("{} failed: {}", plugin.display(), output.status));
    }
    let response =
        CodeGeneratorResponse::decode(output.stdout.as_slice()).map_err(|e| e.to_string())?;
    if let Some(error) = response.error {
        return Err(error);
    }
    let out_dir = root.join(OUTPUT);
    std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let mut written = Vec::new();
    for file in response.file {
        let name = file.name.ok_or("plugin returned a file without a name")?;
        let content = file.content.unwrap_or_default();
        std::fs::write(out_dir.join(&name), content).map_err(|e| e.to_string())?;
        written.push(format!("{OUTPUT}/{name}"));
    }
    Ok(written)
}
