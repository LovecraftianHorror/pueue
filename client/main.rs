use ::anyhow::Result;
use ::simplelog::{Config, LevelFilter, SimpleLogger};
use ::structopt::StructOpt;

use ::pueue::settings::Settings;

pub mod cli;
pub mod client;
pub mod commands;
pub mod output;
pub mod output_helper;

use crate::cli::{Opt, SubCommand};
use crate::client::Client;

#[async_std::main]
async fn main() -> Result<()> {
    let settings = Settings::new()?;
    let save_result = settings.save();
    if save_result.is_err() {
        println!("Failed saving config file.");
        println!("{:?}", save_result.err());
    }

    // Parse commandline options.
    let opt = Opt::from_args();

    // Set the verbosity level for the client app.
    if opt.verbose >= 3 {
        SimpleLogger::init(LevelFilter::Debug, Config::default())?;
    } else if opt.verbose == 2 {
        SimpleLogger::init(LevelFilter::Info, Config::default())?;
    } else if opt.verbose == 1 {
        SimpleLogger::init(LevelFilter::Warn, Config::default())?;
    } else if opt.verbose == 0 {
        SimpleLogger::init(LevelFilter::Error, Config::default())?;
    }

    if let SubCommand::Completions {
        shell,
        output_directory,
    } = &opt.cmd
    {
        let mut clap = Opt::clap();
        clap.gen_completions("pueue", *shell, output_directory);
        return Ok(());
    }

    // Create client to talk with the daemon and connect.
    let client = Client::new(settings, opt).await?;
    client.start().await?;

    Ok(())
}
