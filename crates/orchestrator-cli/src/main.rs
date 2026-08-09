use clap::Parser;

/// Orchestrator: global-hotkey input automation (profile commands not yet implemented).
#[derive(Parser)]
#[command(version, about)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}
