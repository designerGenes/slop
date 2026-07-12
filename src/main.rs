fn main() {
    let args: Vec<String> = std::env::args().collect();

    let silent = args.iter().any(|a| a == "--silent" || a == "-S");

    // Handle -v/--version and -V flags
    if args.len() == 2 && (args[1] == "-v" || args[1] == "-V" || args[1] == "--version") {
        if !silent {
            slop::logo::print_logo_with_version();
        } else {
            println!("slop {}", env!("CARGO_PKG_VERSION"));
        }
        std::process::exit(0);
    }

    // Handle `slop sync` — ensures Sharktopus rules match config without
    // sloping or desloping any files. Intended to be called by a launchd
    // timer so config.yaml changes propagate to Sharktopus live.
    // Supports `slop sync` and `slop --silent sync` forms.
    let sync_position = args
        .iter()
        .position(|a| a == "sync")
        .filter(|&i| i >= 1);
    if let Some(sync_index) = sync_position {
        let only_flags_before = args[1..sync_index]
            .iter()
            .all(|a| a.starts_with('-'));
        if only_flags_before {
            match slop::sync() {
                Ok(messages) => {
                    if !silent {
                        for msg in &messages {
                            eprintln!("{msg}");
                        }
                    }
                }
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            }
            return;
        }
    }

    if let Err(error) = slop::run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
