// The target triple this binary was built for, so an update can be
// matched to it: a release names the target it was built for.
fn main() {
    let target = std::env::var("TARGET").expect("cargo sets TARGET for build scripts");
    println!("cargo:rustc-env=HALOID_BUILD_TARGET={target}");
}
