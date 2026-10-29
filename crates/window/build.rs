//! Compiles the shaders to SPIR-V with glslc, which the Vulkan SDK and the shaderc
//! packages install.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let glslc = std::env::var_os("GLSLC").unwrap_or_else(|| "glslc".into());
    println!("cargo::rerun-if-changed=shaders");
    println!("cargo::rerun-if-env-changed=GLSLC");
    for (source, stage) in [("cells.vert", "vert"), ("cells.frag", "frag")] {
        let status = Command::new(&glslc)
            .arg(format!("-fshader-stage={stage}"))
            .args(["--target-env=vulkan1.3", "-O", "-Werror"])
            .arg(PathBuf::from("shaders").join(source))
            .arg("-o")
            .arg(out.join(format!("{source}.spv")))
            .status();
        match status {
            Ok(status) if status.success() => {}
            Ok(status) => panic!("glslc failed on shaders/{source}: {status}"),
            Err(error) => panic!(
                "running {}: {error}; install glslc (shaderc) or name it in GLSLC",
                glslc.to_string_lossy()
            ),
        }
    }
}
