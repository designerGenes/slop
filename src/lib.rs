pub mod cli;
pub mod config;
pub mod deslop;
pub mod error;
pub mod graph;
pub mod graphstore;
pub mod logo;
pub mod manual_deslop;
pub mod models;
pub mod open;
pub mod page;
pub mod pathing;
pub mod project_graph;
pub mod repomap;
pub mod rules_manifest;
pub mod secrets;
pub mod selection;
pub mod sharktopus;
pub mod slop;
pub mod slop_format;
pub mod slopignore;
pub mod tower_graph;
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

    // Graph modes produce artifacts, not slops, and return before any of the
    // bundling machinery runs.
    if args.project_graph {
        let artifacts = project_graph::run_project_graph(args, config)?;
        if args.show_output_dir {
            if let Some(directory) = artifacts.first().and_then(|path| path.parent()) {
                if let Err(error) = open::open_output_dir_with(opener, directory) {
                    return Err(SlopError::OpenDirectoryFailure {
                        directory: directory.to_path_buf(),
                        message: error.to_string(),
                    });
                }
            }
        }
        return Ok(());
    }
    if args.tower_graph {
        let artifacts = tower_graph::run_tower_graph(args, config)?;
        if args.show_output_dir
            && let Some(directory) = artifacts.first().and_then(|path| path.parent())
        {
            open::open_output_dir_with(opener, directory).map_err(|error| {
                SlopError::OpenDirectoryFailure {
                    directory: directory.to_path_buf(),
                    message: error.to_string(),
                }
            })?;
        }
        return Ok(());
    }
    if args.page_open || args.page_add || args.page_close || args.page_list || args.page_prune {
        page::run(args, config)?;
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
