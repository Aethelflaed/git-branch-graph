use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

mod repository;
use repository::Repository;

#[derive(Default, Parser)]
#[command(version, infer_subcommands = true)]
pub struct Cli {
    #[clap(flatten)]
    pub verbose: clap_verbosity_flag::Verbosity,

    /// Path to the git repository
    #[arg(short = 'C', long, value_name = "PATH")]
    pub directory: Option<PathBuf>,

    /// Branches
    pub branches: Vec<String>,
}

fn main() -> Result<()> {
    use clap::error::ErrorKind::*;

    let cli = match Cli::try_parse_from(std::env::args_os()) {
        Ok(cli) => cli,
        Err(e) => match e.kind() {
            DisplayHelp | DisplayVersion => {
                println!("{}", e);
                return Ok(());
            }
            _ => {
                return Err(e.into());
            }
        },
    };

    setup_log(cli.verbose.log_level_filter())?;

    Repository::try_from(cli)?.run()
}

fn setup_log(level: log::LevelFilter) -> Result<()> {
    use env_logger::{Builder, Env};
    use systemd_journal_logger::{connected_to_journal, JournalLog};

    // If the output streams of this process are directly connected to the
    // systemd journal log directly to the journal to preserve structured
    // log entries (e.g. proper multiline messages, metadata fields, etc.)
    if connected_to_journal() {
        JournalLog::new()
            .unwrap()
            .with_extra_fields(vec![("VERSION", env!("CARGO_PKG_VERSION"))])
            .install()?;
    } else {
        let name = String::from(env!("CARGO_PKG_NAME"))
            .replace('-', "_")
            .to_uppercase();
        let env = Env::new()
            .filter(format!("{}_LOG", name))
            .write_style(format!("{}_LOG_STYLE", name));

        Builder::new()
            .filter_level(log::LevelFilter::Trace)
            .parse_env(env)
            .try_init()?;
    }

    log::set_max_level(level);

    Ok(())
}
