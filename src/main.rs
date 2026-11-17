mod app;
mod config;
mod highlight;
mod input;
mod model;
mod mouse;
mod storage;
mod ui;

use anyhow::{Context, Result, bail};
use clap::Parser;
use std::{io::IsTerminal, path::PathBuf};

#[derive(Debug, Parser)]
#[command(version, about = "Compare and merge two folders in your terminal")]
struct Args {
    /// Left folder (omit both folders for interactive path entry)
    #[arg(value_name = "LEFT", conflicts_with = "left")]
    left_path: Option<PathBuf>,
    /// Right folder
    #[arg(value_name = "RIGHT", conflicts_with = "right")]
    right_path: Option<PathBuf>,
    #[arg(short = 'l', long, value_name = "PATH", conflicts_with_all = ["left_path", "right_path"])]
    left: Option<PathBuf>,
    #[arg(short = 'r', long, value_name = "PATH", conflicts_with_all = ["left_path", "right_path"])]
    right: Option<PathBuf>,
    /// Lua configuration (default: $XDG_CONFIG_HOME/lazymerge/config.lua)
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// Print the default Lua configuration and exit
    #[arg(long)]
    print_config: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.print_config {
        print!("{}", include_str!("../config.example.lua"));
        return Ok(());
    }
    let config = config::Config::load(args.config.as_deref())?;
    let left = args.left.or(args.left_path);
    let right = args.right.or(args.right_path);
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("LazyMerge needs an interactive terminal. Use --help for usage.");
    }
    let mut app = app::App::new(config, left, right)?;
    // ratatui restores raw mode and the alternate screen on errors and panics.
    ratatui::run(|terminal| app.run(terminal)).context("LazyMerge failed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_startup_forms_and_rejects_mixed_paths() {
        for args in [
            vec!["lazymerge"],
            vec!["lazymerge", "a b", "c"],
            vec!["lazymerge", "-l", "a", "-r", "b"],
        ] {
            assert!(Args::try_parse_from(args).is_ok());
        }
        assert!(Args::try_parse_from(["lazymerge", "a", "b", "-l", "c"]).is_err());
    }
}
