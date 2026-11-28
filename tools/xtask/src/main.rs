//! `cargo xtask`: builds the Rust drivers and assembles the driver packages.
//!
//! std only, no external dependencies. See `docs/rust-port/packaging.md`.
//!
//! ```text
//! cargo xtask build   [--arch x64|arm64|all] [--release]
//! cargo xtask package [--arch x64|arm64|all] [--release]
//!                     [--driver-version a.b.c.d] [--sign <signtool sign args...>]
//! cargo xtask test
//! ```

mod cli;
mod drivers;
mod package;
mod paths;
mod process;
mod wdk;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    match cli::parse(args)? {
        cli::Command::Help => {
            print!("{}", cli::USAGE);
            Ok(())
        }
        cli::Command::Build(opts) => {
            let build = drivers::DriverBuild {
                release: opts.release,
                package_version: None,
            };
            drivers::build_drivers(&opts.arches, build)?;
            for &arch in &opts.arches {
                println!(
                    "xtask: [{}] drivers/km -> {}",
                    arch.name(),
                    build.output_dir(arch).display()
                );
            }
            Ok(())
        }
        cli::Command::Package(opts) => package::package(&opts),
        cli::Command::Test => drivers::test_all(),
    }
}
