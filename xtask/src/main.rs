//! `cargo xtask <task>`: development tasks that are not part of the build.

mod keymap;
mod swift_proto;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "usage: cargo xtask <task>

tasks:
  keymap                regenerate crates/iw-keymap/src/table.rs and the
                        apps' key code tables from
                        third_party/chromium/dom_code_data.inc
  swift-proto <plugin>  regenerate the macOS app's Swift protobuf code with
                        swift-protobuf's protoc-gen-swift at <plugin>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("keymap") => keymap_task(),
        Some("swift-proto") => match args.get(1) {
            Some(plugin) => swift_proto_task(Path::new(plugin)),
            None => Err(USAGE.to_owned()),
        },
        _ => Err(USAGE.to_owned()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("xtask: {message}");
            ExitCode::FAILURE
        }
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives inside the workspace")
        .to_path_buf()
}

fn keymap_task() -> Result<(), String> {
    let root = root();
    let source = std::fs::read_to_string(root.join(keymap::SOURCE))
        .map_err(|e| format!("cannot read {}: {e}", keymap::SOURCE))?;
    for (path, text) in keymap::outputs(&source)? {
        let file = root.join(path);
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("cannot create the folder of {path}: {e}"))?;
        }
        std::fs::write(&file, text).map_err(|e| format!("cannot write {path}: {e}"))?;
        println!("wrote {path}");
    }
    Ok(())
}

fn swift_proto_task(plugin: &Path) -> Result<(), String> {
    for file in swift_proto::generate(&root(), plugin)? {
        println!("wrote {file}");
    }
    Ok(())
}
