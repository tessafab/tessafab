//! tessafab: the command-line tool and slicer hook; see tessafab_cli.
//! Built from this source it patterns one piece on one plate.

use tessafab_core::edition::Edition;

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let lib = tessafab_cli::default_library();
    std::process::exit(tessafab_cli::main_with(&argv, &lib, Edition::Source));
}
