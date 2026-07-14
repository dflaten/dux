use std::env;
use std::fs;

fn main() {
    println!("cargo:rerun-if-changed=DEV_VERSION");
    println!("cargo:rerun-if-env-changed=DUX_RELEASE_BUILD");

    let display_version = if env::var("DUX_RELEASE_BUILD").as_deref() == Ok("1") {
        format!("v{}", env!("CARGO_PKG_VERSION"))
    } else {
        fs::read_to_string("DEV_VERSION")
            .expect("read DEV_VERSION")
            .trim()
            .to_string()
    };

    println!("cargo:rustc-env=DUX_DISPLAY_VERSION={display_version}");
}
