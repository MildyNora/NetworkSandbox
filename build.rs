use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=NETSANDBOX_EMBED_LINUX_GUEST");
    println!("cargo:rustc-check-cfg=cfg(netsandbox_embedded_linux_guest)");

    let Some(source) = env::var_os("NETSANDBOX_EMBED_LINUX_GUEST") else {
        return;
    };
    let source = PathBuf::from(source);
    if !source.is_file() {
        panic!(
            "NETSANDBOX_EMBED_LINUX_GUEST is not a regular file: {}",
            source.display()
        );
    }
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set"))
        .join("netsandbox-linux-guest");
    fs::copy(&source, &output).unwrap_or_else(|error| {
        panic!(
            "could not embed Linux guest helper {}: {error}",
            source.display()
        )
    });
    println!("cargo:rerun-if-changed={}", source.display());
    println!("cargo:rustc-cfg=netsandbox_embedded_linux_guest");
}
