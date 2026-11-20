use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=memory.x");
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("arm") {
        let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
        let layout = fs::read_to_string("memory.x").unwrap();
        let layout = if env::var_os("CARGO_FEATURE_BOARD_LIONEL").is_some() {
            layout.replace("4M - 8K", "16M - 8K")
        } else {
            layout
        };
        fs::write(out.join("memory.x"), layout).unwrap();
        println!("cargo:rustc-link-search={}", out.display());
    }
}
