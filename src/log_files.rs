use chrono::NaiveDate;
use std::path::{Path, PathBuf};

const LOG_FILE_PREFIX: &str = "st0x-rest-api.log";

#[derive(Debug, Clone)]
pub struct LogFiles {
    root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct LogFile {
    path: PathBuf,
    download_filename: String,
}

impl LogFiles {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn file_for_date(&self, date: NaiveDate) -> LogFile {
        let formatted_date = date.format("%Y-%m-%d");
        LogFile {
            path: self
                .root
                .join(format!("{LOG_FILE_PREFIX}.{formatted_date}")),
            download_filename: format!("st0x-rest-api-{formatted_date}.log"),
        }
    }
}

impl LogFile {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn download_filename(&self) -> &str {
        &self.download_filename
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_expected_daily_log_path_and_filename() {
        let logs = LogFiles::new("/tmp/st0x-logs");
        let date = NaiveDate::from_ymd_opt(2026, 3, 13).expect("valid test date");

        let file = logs.file_for_date(date);

        assert_eq!(
            file.path(),
            Path::new("/tmp/st0x-logs/st0x-rest-api.log.2026-03-13")
        );
        assert_eq!(file.download_filename(), "st0x-rest-api-2026-03-13.log");
    }
}
