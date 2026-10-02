use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let ui_out = out.join("ui");
    fs::create_dir_all(&ui_out).expect("create the compiled ui directory");

    let mut blps: Vec<PathBuf> = fs::read_dir("data/ui")
        .expect("read data/ui")
        .map(|e| e.expect("read data/ui").path())
        .filter(|p| p.extension().is_some_and(|e| e == "blp"))
        .collect();
    blps.sort();
    let status = Command::new("blueprint-compiler")
        .arg("batch-compile")
        .arg(&ui_out)
        .arg("data/ui")
        .args(&blps)
        .status()
        .expect("blueprint-compiler not found; install it (dnf install blueprint-compiler)");
    assert!(status.success(), "blueprint-compiler failed");

    // gresource references ui/*.ui relative to the compiled ui dir, plus icons from data/.
    let resource_dir = out.join("resources");
    fs::create_dir_all(&resource_dir).expect("create the resource directory");
    copy_dir("data/icons", &resource_dir.join("icons"));
    copy_dir(ui_out, &resource_dir.join("ui"));
    fs::copy(
        "data/caret.gresource.xml",
        resource_dir.join("caret.gresource.xml"),
    )
    .expect("copy data/caret.gresource.xml");

    glib_build_tools::compile_resources(
        &[&resource_dir],
        resource_dir
            .join("caret.gresource.xml")
            .to_str()
            .expect("OUT_DIR is UTF-8"),
        "caret.gresource",
    );

    println!("cargo:rerun-if-changed=data/ui");
    println!("cargo:rerun-if-changed=data/icons");
    println!("cargo:rerun-if-changed=data/caret.gresource.xml");
}

fn copy_dir(src: impl AsRef<std::path::Path>, dst: &std::path::Path) {
    fs::create_dir_all(dst).expect("create a resource directory");
    for e in fs::read_dir(src).expect("read a data directory") {
        let e = e.expect("read a data directory");
        let to = dst.join(e.file_name());
        if e.file_type().expect("stat a data file").is_dir() {
            copy_dir(e.path(), &to);
        } else {
            fs::copy(e.path(), to).expect("copy a data file");
        }
    }
}
