mod dev;
mod user;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "lil", about = "LILITH command line")]
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
        Command::Dev(_) if cli.global.any_set() => {
            eprintln!("`lil dev` doesn't take --workspace, --non-interactive or --no-start.");
            2
        }
        Command::Dev(cmd) => dev::run(cmd),
        Command::User(cmd) => user::run(cmd, &cli.global),
    };
    std::process::exit(code);
}
