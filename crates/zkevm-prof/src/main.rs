//! Profiles stateless validator guests on zkVMs and publishes the results.
//!
//! `profile` runs one guest over a corpus of EEST fixtures and records the cost the zkVM charges
//! for each execution. `index` lists the runs published under a directory, which is what the page
//! reads to know what it can load.

mod command;
mod fixture;
mod profile;
mod registry;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::command::{index::IndexCmd, profile::ProfileCmd};

#[derive(Parser)]
#[command(
    name = "zkevm-prof",
    about = "Profiles stateless validator guests on zkVMs"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Profiles a guest over every fixture in a directory.
    Profile(ProfileCmd),
    /// Lists the published profiles for the page.
    Index(IndexCmd),
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Profile(cmd) => cmd.run().await,
        Command::Index(cmd) => cmd.run(),
    }
}
