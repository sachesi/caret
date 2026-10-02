use std::path::PathBuf;
use std::process::Command;
use std::{env, fs};

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let ui_dir = out_dir.join("ui");
    fs::create_dir_all(&ui_dir).expect("create the compiled ui directory");

    let mut blueprints: Vec<PathBuf> = fs::read_dir("data/ui")
        .expect("read data/ui")
        .map(|entry| entry.expect("read data/ui").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "blp"))
        .collect();
    blueprints.sort();
    let status = Command::new("blueprint-compiler")
        .arg("batch-compile")
        .arg(&ui_dir)
        .arg("data/ui")
        .args(&blueprints)
        .status()
        .expect("run blueprint-compiler; install it (dnf install blueprint-compiler)");
    assert!(status.success(), "blueprint-compiler failed");
    println!("cargo:rerun-if-changed=data/ui");

    // The gresource lists ui/*.ui, found in OUT_DIR, beside files kept in data/.
    glib_build_tools::compile_resources(
        &[PathBuf::from("data"), out_dir],
        "data/resources.gresource.xml",
        "resources.gresource",
    );
}
