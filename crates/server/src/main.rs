//! tessafab-engine serve: the Community engine for the desktop app
//! (JSON-RPC 2.0 over stdio, see tessafab_core::rpc). Only protocol
//! messages go to stdout; anything else goes to stderr.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("serve") => {
            let stdin = std::io::stdin();
            tessafab_core::rpc::serve(stdin.lock(), Box::new(std::io::stdout()), None);
        }
        Some("--version") => println!("{}", tessafab_core::VERSION),
        _ => {
            eprintln!("usage: tessafab-engine serve");
            std::process::exit(2);
        }
    }
}
