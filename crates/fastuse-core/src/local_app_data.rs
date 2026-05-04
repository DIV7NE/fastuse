//! `%LOCALAPPDATA%\fastuse\` resolution + creation.

use std::path::PathBuf;

/// Return `%LOCALAPPDATA%\fastuse\`, creating it on first call.
///
/// Falls back to `%TEMP%\fastuse` if `LOCALAPPDATA` is unset (extremely rare on
/// modern Windows, but we don't want to panic).
pub fn local_app_data() -> std::io::Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("TEMP").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows\Temp"));
    let dir = base.join("fastuse");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_app_data_creates_dir() {
        let p = local_app_data().expect("LOCALAPPDATA must resolve");
        assert!(p.exists());
        assert!(p.ends_with("fastuse"));
    }
}
