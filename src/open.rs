use std::fs;
use std::path::Path;
use std::process::Command;

use crate::error::SlopError;

pub trait OutputDirOpener {
    fn open(&self, path: &Path) -> Result<(), SlopError>;
}

#[derive(Debug, Default)]
pub struct SystemOutputDirOpener;

pub fn open_output_dir_with(
    opener: &impl OutputDirOpener,
    path: &Path,
) -> Result<(), SlopError> {
    opener.open(path)
}

impl OutputDirOpener for SystemOutputDirOpener {
    fn open(&self, path: &Path) -> Result<(), SlopError> {
        if let Some(mock_file) = std::env::var_os("slop_OPEN_MOCK_FILE") {
            fs::write(&mock_file, format!("{}\n", path.display())).map_err(|error| {
                SlopError::OpenDirectoryFailure {
                    directory: path.to_path_buf(),
                    message: error.to_string(),
                }
            })?;
        }

        if std::env::var_os("slop_OPEN_FORCE_FAIL").is_some() {
            return Err(SlopError::OpenDirectoryFailure {
                directory: path.to_path_buf(),
                message: "forced open failure".to_string(),
            });
        }

        if std::env::var_os("slop_OPEN_MOCK_FILE").is_some() {
            return Ok(());
        }

        let command = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(target_os = "windows") {
            "explorer"
        } else {
            "xdg-open"
        };

        let status = Command::new(command).arg(path).status().map_err(|error| {
            SlopError::OpenDirectoryFailure {
                directory: path.to_path_buf(),
                message: error.to_string(),
            }
        })?;

        if status.success() {
            Ok(())
        } else {
            Err(SlopError::OpenDirectoryFailure {
                directory: path.to_path_buf(),
                message: format!("{command} exited with status {status}"),
            })
        }
    }
}
