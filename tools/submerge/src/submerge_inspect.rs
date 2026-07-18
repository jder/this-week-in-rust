use anyhow::{Result, bail};
use clap::Parser;
use std::path::PathBuf;
use submerge::validation;

#[derive(Debug, Parser)]
#[command(
    name = "submerge-inspect",
    about = "Validate TWiR Markdown and links without invoking Python"
)]
struct Args {
    /// Markdown files to inspect. When omitted, inspect recent files from --paths.
    #[arg(long)]
    file: Vec<PathBuf>,

    /// Directory paths to inspect, separated with colons.
    #[arg(long, default_value = "content:draft")]
    paths: String,

    /// Number of most-recent matching files to inspect.
    #[arg(long, default_value_t = 25)]
    num_recent: usize,

    /// Print warnings as well as errors. Warnings do not affect the exit status.
    #[arg(long)]
    show_warnings: bool,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_target(false)
        .init();
    let args = Args::parse();
    let files = if args.file.is_empty() {
        validation::recent_files(&args.paths, args.num_recent)?
    } else {
        args.file
    };
    let report = validation::inspect_files(&files)?;
    for error in &report.errors {
        println!("* error: {error}");
    }
    if args.show_warnings {
        for warning in &report.warnings {
            println!("* warning: {warning}");
        }
    }
    if report.errors.is_empty() {
        Ok(())
    } else {
        bail!("validation found {} error(s)", report.errors.len())
    }
}
