#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;

use etherquic_installers::{build_android_package, build_windows_installer, Error};

fn main() -> ExitCode {
    match build() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("etherquic-installers: {error}");
            ExitCode::FAILURE
        }
    }
}

fn build() -> Result<(), Error> {
    let mut args = std::env::args().skip(1);
    let output = args
        .next()
        .ok_or_else(|| Error::message("usage: etherquic-installers <output-directory>"))?;
    if args.next().is_some() {
        return Err(Error::message(
            "usage: etherquic-installers <output-directory>",
        ));
    }
    let output = PathBuf::from(output);
    std::fs::create_dir_all(&output)?;
    build_windows_installer(&output.join("etherquic-play-1.0.0.msi"), "1.0.0")?;
    build_windows_installer(&output.join("etherquic-play-1.1.0.msi"), "1.1.0")?;
    build_android_package(&output.join("etherquic-play-1.apk"), 1, "1.0.0")?;
    build_android_package(&output.join("etherquic-play-2.apk"), 2, "1.1.0")?;
    println!("wrote {}", output.display());
    Ok(())
}
