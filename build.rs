fn main() {
    println!("cargo:rerun-if-changed=nightsleep.rc");
    println!("cargo:rerun-if-changed=resources");

    // Resources (icons, manifest, version info) only make sense for Windows targets.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("nightsleep.rc", embed_resource::NONE)
            .manifest_required()
            .expect("failed to compile Windows resources");
    }
}
