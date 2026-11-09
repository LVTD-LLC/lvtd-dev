fn main() {
    indexnow_revision();
    println!("cargo:rerun-if-changed=content/blog");
    println!("cargo:rerun-if-changed=content/docs");
}

// Source identity for automatic IndexNow notifications, including non-Docker builds.
fn indexnow_revision() {
    use std::hash::{Hash, Hasher};
    fn visit(path: &std::path::Path, hash: &mut std::collections::hash_map::DefaultHasher) {
        let mut entries: Vec<_> = std::fs::read_dir(path).expect("app sources").map(|e| e.unwrap().path()).collect();
        entries.sort();
        for entry in entries {
            entry.hash(hash);
            if entry.is_dir() {
                visit(&entry, hash);
            } else {
                std::fs::read(entry).expect("source file").hash(hash);
            }
        }
    }
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    for path in ["src", "assets", "content"] {
        println!("cargo:rerun-if-changed={path}");
        visit(std::path::Path::new(path), &mut hash);
    }
    let mut features: Vec<_> = std::env::vars().filter(|(k, _)| k.starts_with("CARGO_FEATURE_")).collect();
    features.sort();
    features.hash(&mut hash);
    println!("cargo:rerun-if-env-changed=BUILD_REVISION");
    std::env::var("BUILD_REVISION").unwrap_or_default().hash(&mut hash);
    println!("cargo:rustc-env=INDEXNOW_REVISION={:016x}", hash.finish());
}
