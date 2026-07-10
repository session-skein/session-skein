use std::path::PathBuf;

use clap::Args;
use clap::Parser;
use clap::Subcommand;
use serde::Serialize;
use skein_core::Registry;
use skein_core::SkeinPaths;

#[derive(Debug, Parser)]
#[command(name = "skein", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show paths, state, and schema health without modifying anything.
    Doctor(OutputArgs),
    /// Initialize the private local state database.
    Init(OutputArgs),
    /// Manage explicitly registered projects.
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },
}

#[derive(Debug, Args)]
struct OutputArgs {
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum ProjectCommand {
    /// Add or update a project by its canonical path.
    Add {
        /// Existing project directory.
        path: PathBuf,
        /// Override the directory-derived display name.
        #[arg(long)]
        name: Option<String>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// List projects in stable display order.
    List(OutputArgs),
}

#[derive(Serialize)]
struct DoctorReport {
    version: &'static str,
    config_dir: PathBuf,
    data_dir: PathBuf,
    database: PathBuf,
    database_exists: bool,
    schema_version: Option<i64>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let paths = SkeinPaths::discover()?;

    match cli.command {
        Command::Doctor(output) => doctor(&paths, output.json)?,
        Command::Init(output) => {
            let registry = Registry::open(&paths)?;
            let report = report(&paths, Some(registry.schema_version()?));
            print_value(&report, output.json)?;
        }
        Command::Project { command } => {
            let registry = Registry::open(&paths)?;
            match command {
                ProjectCommand::Add { path, name, json } => {
                    let project = registry.add_project(&path, name.as_deref())?;
                    print_value(&project, json)?;
                }
                ProjectCommand::List(output) => {
                    let projects = registry.list_projects()?;
                    print_value(&projects, output.json)?;
                }
            }
        }
    }

    Ok(())
}

fn doctor(paths: &SkeinPaths, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let database_exists = paths.database().is_file();
    let schema_version = if database_exists {
        Some(Registry::open_read_only(paths)?.schema_version()?)
    } else {
        None
    };
    print_value(&report(paths, schema_version), json)
}

fn report(paths: &SkeinPaths, schema_version: Option<i64>) -> DoctorReport {
    DoctorReport {
        version: env!("CARGO_PKG_VERSION"),
        config_dir: paths.config_dir.clone(),
        data_dir: paths.data_dir.clone(),
        database: paths.database(),
        database_exists: paths.database().is_file(),
        schema_version,
    }
}

fn print_value<T: Serialize>(value: &T, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        println!("{}", serde_json::to_string(value)?);
    }
    Ok(())
}
