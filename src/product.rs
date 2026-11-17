pub const NAME: &str = "codex-AIs";
/// Cargo needs three version components; the product omits a zero patch.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
        .strip_suffix(".0")
        .unwrap_or(env!("CARGO_PKG_VERSION"))
}
