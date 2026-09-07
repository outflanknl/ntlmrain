use std::{fs, path::PathBuf};

const ASSETS: &[&str] = &[
    "precompute_compact.wgsl",
    "precompute_expanded.wgsl",
    "verify_compact.wgsl",
    "verify_expanded.wgsl",
    "des_lut.bin",
];

#[test]
fn embedded_assets_match_web2_sources_when_present() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let web2 = manifest.join("../web2/shaders");
    if !web2.is_dir() {
        // A standalone source archive intentionally has no sibling Web2 tree.
        return;
    }
    for name in ASSETS {
        let embedded = fs::read(manifest.join("shaders").join(name)).unwrap();
        let source = fs::read(web2.join(name)).unwrap();
        assert_eq!(embedded, source, "embedded asset {name} is stale");
    }
}
