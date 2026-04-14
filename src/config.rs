use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub log_dir: String,
    pub database_url: String,
    pub registry_url: String,
    pub rate_limit_global_rpm: u64,
    pub rate_limit_per_key_rpm: u64,
    pub docs_dir: String,
    pub local_db_path: String,
    /// TTL (seconds) for gating signatures produced by the API. Each swap
    /// calldata response embeds a signed context whose `expiry` field is
    /// `now() + this`. Keep short enough that a captured signature is not
    /// useful for long. The gating signer private key is read from the
    /// `ST0X_GATING_SIGNER_KEY` env var and never appears in this file.
    pub gating_signature_ttl_seconds: u64,
}

/// Upper bound on `gating_signature_ttl_seconds`. A captured signature is
/// replayable until `expiry`, so the window is a security control.
const GATING_SIGNATURE_TTL_MAX_SECONDS: u64 = 300;

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let contents =
            std::fs::read_to_string(path).map_err(|e| format!("failed to read config: {e}"))?;
        let cfg: Self =
            toml::from_str(&contents).map_err(|e| format!("failed to parse config: {e}"))?;
        if cfg.gating_signature_ttl_seconds == 0 {
            return Err("gating_signature_ttl_seconds must be > 0".into());
        }
        if cfg.gating_signature_ttl_seconds > GATING_SIGNATURE_TTL_MAX_SECONDS {
            return Err(format!(
                "gating_signature_ttl_seconds must be <= {GATING_SIGNATURE_TTL_MAX_SECONDS}"
            ));
        }
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    struct TempCfg(PathBuf);
    impl Drop for TempCfg {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn write_config(body: &str) -> TempCfg {
        let path = std::env::temp_dir().join(format!("st0x-cfg-{}.toml", uuid::Uuid::new_v4()));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        TempCfg(path)
    }

    fn base_config(ttl: &str) -> String {
        format!(
            r#"log_dir = "/tmp/l"
database_url = "sqlite::memory:"
registry_url = "https://example.com/r"
rate_limit_global_rpm = 10
rate_limit_per_key_rpm = 1
docs_dir = "/tmp/d"
local_db_path = "/tmp/db"
gating_signature_ttl_seconds = {ttl}
"#
        )
    }

    #[test]
    fn rejects_zero_ttl() {
        let f = write_config(&base_config("0"));
        let err = Config::load(&f.0).unwrap_err();
        assert!(err.contains("must be > 0"), "got: {err}");
    }

    #[test]
    fn rejects_ttl_above_max() {
        let f = write_config(&base_config("301"));
        let err = Config::load(&f.0).unwrap_err();
        assert!(err.contains("must be <= 300"), "got: {err}");
    }

    #[test]
    fn accepts_valid_ttl() {
        let f = write_config(&base_config("60"));
        let cfg = Config::load(&f.0).expect("valid config");
        assert_eq!(cfg.gating_signature_ttl_seconds, 60);
    }
}
