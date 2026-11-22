/// The program's name and version, such as `spareline 0.1.0`.
#[must_use]
pub fn version_line() -> String {
    let name = "spareline";
    format!("{name} {}", env!("CARGO_PKG_VERSION"))
}
