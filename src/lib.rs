pub mod cli;
pub mod config;
pub mod deslop;
pub mod error;
pub mod graph;
pub mod logo;
pub mod manual_deslop;
pub mod models;
pub mod open;
pub mod pathing;
pub mod repomap;
pub mod secrets;
pub mod selection;
pub mod sharktopus;
pub mod slop_format;
pub mod slop;
pub mod slopignore;
pub mod tree;

use cli::parse_cli_args;
use config::load_config;
use error::SlopError;
use open::{OutputDirOpener, SystemOutputDirOpener};

pub fn run() -> Result<(), SlopError> {
    if let Err(error) = config::ensure_config_dir() {
        eprintln!("warning: failed to create config directory: {error}");
    }
    let args = parse_cli_args()?;
    let config = load_config();

    if !args.silent {
        logo::print_logo();
    }

    // Sync Sharktopus rules on every invocation so config changes propagate.
    if let Err(error) = sync() {
        if !args.silent {
            eprintln!("warning: failed to sync Sharktopus rules: {error}");
        }
    }

    run_with_opener(&args, &config, &SystemOutputDirOpener)
}

pub fn sync() -> Result<Vec<String>, SlopError> {
    if let Err(error) = config::ensure_config_dir() {
        eprintln!("warning: failed to create config directory: {error}");
    }
    let config = load_config();
    sharktopus::ensure_rules(&config)
}

pub fn run_with_opener(
    args: &models::CliArgs,
    config: &config::Config,
    opener: &impl OutputDirOpener,
) -> Result<(), SlopError> {
    if args.deslop {
        deslop::run_deslop(args, config)?;
        return Ok(());
    }

    let slop_file = slop::run_slop(args, config)?;
    if args.show_output_dir {
        let output_dir = slop_file
            .parent()
            .map(|path| path.to_path_buf())
            .ok_or_else(|| SlopError::OpenDirectoryAfterWriteFailed {
                slop_file: slop_file.clone(),
                directory: slop_file.clone(),
                message: "generated slop file has no parent directory".to_string(),
            })?;

        if let Err(error) = open::open_output_dir_with(opener, &output_dir) {
            return Err(SlopError::OpenDirectoryAfterWriteFailed {
                slop_file,
                directory: output_dir,
                message: error.to_string(),
            });
        }
    }

    Ok(())
}
