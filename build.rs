fn main() {
    // Rebuild when frontend static files change
    println!("cargo:rerun-if-changed=static/");
}
