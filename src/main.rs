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
#[command(
    version,
    about = "Compare and merge two folders or two files in your terminal"
)]
struct Args {
    /// Left folder or file (omit both paths for interactive path entry)
    #[arg(value_name = "LEFT", conflicts_with = "left")]
    left_path: Option<PathBuf>,
    /// Right folder or file; compare two folders or two files
    #[arg(value_name = "RIGHT", conflicts_with = "right")]
    right_path: Option<PathBuf>,
    /// Left folder or file
    #[arg(short = 'l', long, value_name = "PATH", conflicts_with_all = ["left_path", "right_path"])]
    left: Option<PathBuf>,
    /// Right folder or file
    #[arg(short = 'r', long, value_name = "PATH", conflicts_with_all = ["left_path", "right_path"])]
    right: Option<PathBuf>,
    /// Lua configuration file (created automatically at the default path)
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// Print the default Lua configuration and exit
    #[arg(long)]
    print_config: bool,
    /// Restore the default Lua config and exit; use --config FILE for a custom path
    #[arg(long, conflicts_with_all = ["print_config", "left_path", "right_path", "left", "right"])]
    reset_config: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.print_config {
        print!("{}", config::DEFAULT_CONFIG);
        return Ok(());
    }
    if args.reset_config {
        let path = config::Config::reset(args.config.as_deref())?;
        println!("Restored default config: {}", path.display());
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

    #[test]
    fn reset_config_accepts_a_custom_path_and_rejects_startup_or_print_options() {
        assert!(
            Args::try_parse_from(["lazymerge", "--reset-config"])
                .unwrap()
                .reset_config
        );
        let args =
            Args::try_parse_from(["lazymerge", "--reset-config", "--config", "my config.lua"])
                .unwrap();
        assert_eq!(args.config, Some(PathBuf::from("my config.lua")));
        for arguments in [
            vec!["lazymerge", "--reset-config", "--print-config"],
            vec!["lazymerge", "--reset-config", "left", "right"],
            vec!["lazymerge", "--reset-config", "-l", "left"],
            vec!["lazymerge", "--reset-config", "-r", "right"],
        ] {
            assert!(Args::try_parse_from(arguments).is_err());
        }
    }
}
