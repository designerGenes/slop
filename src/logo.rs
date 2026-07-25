pub const LOGO: &str = concat!(
    "\n",
    "     ▗▟████▙▖    ▗▟███▖  ▐█▌     ▗▟███▙▖  ▗▟███▙▖\n",
    "   ▗▟██▀██▀██▙▖  ▐▛▀▀▀▘  ▐█▌     ▐█▌ ▐█▌  ▐█▌ ▐█▌\n",
    "   ▐████▄▄████▌  ▝▀▀██▖  ▐█▌     ▐█▌ ▐█▌  ▐████▛▘\n",
    "   ▝▜████████▛▘  ▝███▛▘  ▐████▖  ▝▜███▛▘  ▐█▌",
);

pub fn logo_with_version() -> String {
    format!("{LOGO}\nslop v{}\n", env!("CARGO_PKG_VERSION"))
}

pub fn print_logo() {
    eprint!("{LOGO}\n\n");
}

pub fn print_logo_with_version() {
    eprint!("{}\n", logo_with_version());
}
