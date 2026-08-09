//! Profiles stateless validator guests on zkVMs and reports the results.
//!
//! `profile` runs one guest over a corpus of EEST fixtures and records the cost the zkVM charges
//! for each execution. `report` aggregates several such runs into a single page.

mod command;
mod fixture;
mod registry;
mod zkvm;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::command::{profile::ProfileCmd, report::ReportCmd};

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
    /// Aggregates profiles into a report.
    Report(ReportCmd),
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Profile(cmd) => cmd.run().await,
        Command::Report(cmd) => cmd.run(),
    }
}
