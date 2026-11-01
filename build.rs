// Embedded resources (icons, stylesheet) must trigger a rebuild when edited.
fn main() {
    println!("cargo:rerun-if-changed=data/icons");
    println!("cargo:rerun-if-changed=data/style.css");
}
