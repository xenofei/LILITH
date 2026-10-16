mod dev;
mod user;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ww", about = "Worldwright command line")]
struct Cli {
    #[command(flatten)]
    global: user::GlobalArgs,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Developer tooling.
    #[command(subcommand)]
    Dev(dev::DevCommand),
    #[command(flatten)]
    User(user::UserCommand),
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Dev(cmd) => dev::run(cmd),
        Command::User(cmd) => user::run(cmd, &cli.global),
    };
    std::process::exit(code);
}
