//! Thin entry point for the native `yunxi` command router.

use std::process;

fn main() {
    match yunxi_launcher::run_from_env() {
        Ok(exit_code) => process::exit(exit_code),
        Err(error) => {
            eprintln!("yunxi launcher error: {error}");
            process::exit(1);
        }
    }
}
