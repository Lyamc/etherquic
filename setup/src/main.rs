//! Install the etherquic host service on this Windows PC.
//!
//! The plan in `plan` is pure. This file only dispatches. Win32 lives in `win`.

mod plan;

#[cfg(windows)]
mod win;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args
        .iter()
        .any(|arg| arg == "--help" || arg == "-h" || arg == "/?")
    {
        println!("{}", plan::USAGE);
        return;
    }
    let cli = match plan::parse_args(&args) {
        Ok(cli) => cli,
        Err(err) => {
            eprintln!("etherquic-setup: {err}");
            eprintln!("{}", plan::USAGE);
            std::process::exit(2);
        }
    };
    #[cfg(windows)]
    {
        match win::execute(&cli) {
            Ok(code) => std::process::exit(code),
            Err(err) => {
                eprintln!("etherquic-setup: {err}");
                std::process::exit(1);
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = cli;
        eprintln!("etherquic-setup installs the Windows host service.");
        std::process::exit(1);
    }
}
