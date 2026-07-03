fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Handle -v/--version and -V flags
    if args.len() == 2 && (args[1] == "-v" || args[1] == "-V" || args[1] == "--version") {
        println!("soupify {}", env!("CARGO_PKG_VERSION"));
        std::process::exit(0);
    }

    // Handle `soupify sync` — ensures Sharktopus rules match config without
    // souping or desouping any files. Intended to be called by a launchd
    // timer so config.yaml changes propagate to Sharktopus live.
    if args.len() == 2 && args[1] == "sync" {
        match soupify::sync() {
            Ok(messages) => {
                for msg in &messages {
                    eprintln!("{msg}");
                }
            }
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }

    if let Err(error) = soupify::run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
