mod dev;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ww", about = "Worldwright command line")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Developer tooling.
    #[command(subcommand)]
    Dev(dev::DevCommand),
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Dev(cmd) => dev::run(cmd),
    };
    std::process::exit(code);
}
