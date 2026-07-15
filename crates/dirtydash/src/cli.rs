use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};

use crate::app_paths::AppPaths;
use crate::collector;
use crate::config::Config;
use crate::db::Database;
use crate::importers::{self, ImportOptions};
use crate::loop_upgrade;
use crate::pricing;
use crate::remote;
use crate::server;

#[derive(Debug, Parser)]
#[command(name = "dirtydash")]
#[command(about = "Local-first AI coding usage dashboard")]
#[command(version)]
pub struct Cli {
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    #[arg(long, global = true, value_name = "PATH")]
    pub db: Option<PathBuf>,

    #[arg(long, global = true, value_name = "KIND=PATH")]
    pub source_root: Vec<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Detect local Claude Code, Codex, OpenCode, Pi, and Hermes sources.
    Scan(ScanArgs),
    /// Import detected or configured local sources into SQLite.
    Import(ImportArgs),
    /// Start the local dashboard server.
    Serve(ServeArgs),
    /// Validate config, database, source paths, parser health, and pricing assumptions.
    Doctor(DoctorArgs),
    /// Run the outbound-only local Collector reconciliation/runtime.
    Collector(CollectorCommand),
    /// Configure pull-based SSH remotes.
    Remote(RemoteCommand),
    /// Inspect bundled pricing and manage manual overrides.
    Pricing(PricingCommand),
    /// Inspect or maintain dirtyloops loop artifacts.
    Loop(LoopCommand),
}

#[derive(Debug, Args)]
pub struct ScanArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    #[arg(long, default_value_t = true)]
    pub metadata_only: bool,

    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args, Clone)]
pub struct ServeArgs {
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,

    #[arg(long, default_value_t = 4599)]
    pub port: u16,

    #[arg(long)]
    pub open: bool,
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CollectorCommand {
    #[command(subcommand)]
    pub command: CollectorSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum CollectorSubcommand {
    /// Reconcile local harness sources and queue one durable outbound batch.
    Reconcile(CollectorReconcileArgs),
    /// Print metadata-only Collector diagnostics.
    Diagnostics(CollectorDiagnosticsArgs),
}

#[derive(Debug, Args)]
pub struct CollectorReconcileArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CollectorDiagnosticsArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct RemoteCommand {
    #[command(subcommand)]
    pub command: RemoteSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum RemoteSubcommand {
    Add(RemoteAddArgs),
    Sync(RemoteSyncArgs),
    List(RemoteListArgs),
    Remove(RemoteRemoveArgs),
}

#[derive(Debug, Args)]
pub struct RemoteAddArgs {
    pub name: String,
    pub ssh_target: String,

    #[arg(long, value_name = "KIND=PATH")]
    pub source_root: Vec<String>,
}

#[derive(Debug, Args)]
pub struct RemoteSyncArgs {
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct RemoteListArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct RemoteRemoveArgs {
    pub name: String,
}

#[derive(Debug, Args)]
pub struct PricingCommand {
    #[command(subcommand)]
    pub command: PricingSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum PricingSubcommand {
    List(PricingListArgs),
    Override(PricingOverrideArgs),
    MarkFree(PricingMarkFreeArgs),
}

#[derive(Debug, Args)]
pub struct PricingListArgs {
    #[arg(long)]
    pub provider: Option<String>,

    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct PricingOverrideArgs {
    #[arg(long)]
    pub provider: String,

    #[arg(long)]
    pub model: String,

    #[arg(long)]
    pub input: f64,

    #[arg(long)]
    pub output: f64,

    #[arg(long)]
    pub cache_read: Option<f64>,

    #[arg(long)]
    pub cache_write: Option<f64>,
}

#[derive(Debug, Args)]
pub struct PricingMarkFreeArgs {
    #[arg(long)]
    pub provider: String,

    #[arg(long)]
    pub model: String,
}

#[derive(Debug, Args)]
pub struct LoopCommand {
    #[command(subcommand)]
    pub command: LoopSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum LoopSubcommand {
    /// Upgrade a dirtyloops-generated loop directory to the current runtime artifacts.
    Upgrade(LoopUpgradeArgs),
}

#[derive(Debug, Args, Clone)]
pub struct LoopUpgradeArgs {
    /// Existing docs/implementation/<stream> loop directory.
    #[arg(value_name = "LOOP_DIR")]
    pub loop_dir: PathBuf,

    /// dirtyloops skill root. Defaults to DIRTYLOOPS_ROOT, ~/.agents/skills/dirtyloops, or ~/dev/agents/skills/dirtyloops.
    #[arg(long, value_name = "PATH")]
    pub dirtyloops_root: Option<PathBuf>,

    /// Report whether the loop is current without writing files. Exits non-zero when upgrades are needed.
    #[arg(long)]
    pub check: bool,

    /// Show planned writes without changing files.
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    run_with_cli(cli)
}

pub fn run_with_cli(cli: Cli) -> Result<()> {
    let paths = AppPaths::resolve(cli.config.clone(), cli.db.clone())?;
    let mut config = Config::load(&paths.config_path)?;
    config.merge_cli_source_roots(&cli.source_root)?;

    match cli.command {
        Some(Command::Scan(args)) => scan(&config, args),
        Some(Command::Import(args)) => import(&paths, &config, args),
        Some(Command::Serve(args)) => serve(&paths, args),
        Some(Command::Doctor(args)) => doctor(&paths, &config, args),
        Some(Command::Collector(args)) => collector_command(&paths, &config, args),
        Some(Command::Remote(args)) => remote::run(&paths, &mut config, args),
        Some(Command::Pricing(args)) => pricing_command(&paths, args),
        Some(Command::Loop(args)) => loop_command(args),
        None => first_run(paths, config),
    }
}

fn loop_command(args: LoopCommand) -> Result<()> {
    match args.command {
        LoopSubcommand::Upgrade(upgrade_args) => loop_upgrade::run(upgrade_args),
    }
}

fn scan(config: &Config, args: ScanArgs) -> Result<()> {
    let sources = importers::scan_sources(config)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&sources)?);
    } else {
        println!("source\tconfidence\tfiles\tpath");
        for source in sources {
            println!(
                "{}\t{}\t{}\t{}",
                source.kind.as_str(),
                source.confidence,
                source.file_count,
                source.path.display()
            );
        }
    }
    Ok(())
}

fn import(paths: &AppPaths, config: &Config, args: ImportArgs) -> Result<()> {
    let db = Database::open(&paths.db_path)?;
    db.migrate()?;
    pricing::seed_bundled_pricing(&db)?;
    let report = importers::import_detected(
        &db,
        config,
        ImportOptions {
            metadata_only: args.metadata_only,
        },
    )?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "imported {} new events and updated {} existing events from {} files ({} parse errors)",
            report.inserted_events,
            report.updated_existing_events,
            report.files_seen,
            report.parse_errors
        );
    }
    Ok(())
}

fn serve(paths: &AppPaths, args: ServeArgs) -> Result<()> {
    let db = Database::open(&paths.db_path)?;
    db.migrate()?;
    pricing::seed_bundled_pricing(&db)?;
    server::serve(paths.db_path.clone(), args)
}

fn doctor(paths: &AppPaths, config: &Config, args: DoctorArgs) -> Result<()> {
    let db = Database::open(&paths.db_path)?;
    db.migrate()?;
    pricing::seed_bundled_pricing(&db)?;
    let report = db.doctor(config)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("config: {}", paths.config_path.display());
        println!("database: {}", paths.db_path.display());
        println!("events: {}", report.event_count);
        println!("pricing records: {}", report.pricing_count);
        println!("detected sources: {}", report.detected_sources);
        if !report.warnings.is_empty() {
            println!("warnings:");
            for warning in report.warnings {
                println!("- {warning}");
            }
        }
    }
    Ok(())
}

fn collector_command(paths: &AppPaths, config: &Config, args: CollectorCommand) -> Result<()> {
    let usage_db = Database::open(&paths.db_path)?;
    let collector_db = Database::open(&paths.collector_db_path)?;
    let mut runtime = collector::Collector::from_config(usage_db, collector_db, config)?;
    match args.command {
        CollectorSubcommand::Reconcile(reconcile_args) => {
            let report = runtime.reconcile_manual(chrono::Utc::now())?;
            if reconcile_args.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "collector reconciled {} files, queued {} events, batch {}",
                    report.files_seen,
                    report.events_queued,
                    report.batch_id.as_deref().unwrap_or("none")
                );
            }
        }
        CollectorSubcommand::Diagnostics(diagnostics_args) => {
            let report = runtime.diagnostics()?;
            if diagnostics_args.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "collector machine={} pending={} watcher_degraded={}",
                    report.machine_id, report.pending_outbox, report.watcher.degraded
                );
            }
        }
    }
    Ok(())
}

fn pricing_command(paths: &AppPaths, args: PricingCommand) -> Result<()> {
    let db = Database::open(&paths.db_path)?;
    db.migrate()?;
    pricing::seed_bundled_pricing(&db)?;
    match args.command {
        PricingSubcommand::List(list_args) => {
            let rows = pricing::list_pricing(&db, list_args.provider.as_deref())?;
            if list_args.json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                println!("provider\tmodel\tinput\toutput\tcache_read\tcache_write\toverride\tfree");
                for row in rows {
                    println!(
                        "{}\t{}\t{:.4}\t{:.4}\t{:.4}\t{:.4}\t{}\t{}",
                        row.provider,
                        row.model,
                        row.input_rate,
                        row.output_rate,
                        row.cache_read_rate,
                        row.cache_write_rate,
                        row.override_flag,
                        row.local_free_flag
                    );
                }
            }
        }
        PricingSubcommand::Override(override_args) => {
            pricing::override_price(
                &db,
                &override_args.provider,
                &override_args.model,
                override_args.input,
                override_args.output,
                override_args.cache_read,
                override_args.cache_write,
            )?;
            println!(
                "overrode pricing for {}/{}",
                override_args.provider, override_args.model
            );
        }
        PricingSubcommand::MarkFree(mark_free_args) => {
            pricing::mark_free(&db, &mark_free_args.provider, &mark_free_args.model)?;
            println!(
                "marked {}/{} as local/free",
                mark_free_args.provider, mark_free_args.model
            );
        }
    }
    Ok(())
}

fn first_run(paths: AppPaths, config: Config) -> Result<()> {
    let db = Database::open(&paths.db_path)?;
    db.migrate()?;
    pricing::seed_bundled_pricing(&db)?;

    let sources = importers::scan_sources(&config)?;
    println!("dirtydash first-run scan");
    println!("source\tconfidence\tfiles\tpath");
    for source in &sources {
        println!(
            "{}\t{}\t{}\t{}",
            source.kind.as_str(),
            source.confidence,
            source.file_count,
            source.path.display()
        );
    }

    let report = importers::import_sources(
        &db,
        sources,
        ImportOptions {
            metadata_only: true,
        },
    )
    .context("first-run import failed")?;
    println!(
        "metadata-only import complete: {} new events and {} updated events from {} files",
        report.inserted_events, report.updated_existing_events, report.files_seen
    );

    let args = ServeArgs {
        host: "127.0.0.1".to_string(),
        port: 4599,
        open: false,
    };
    server::serve(paths.db_path, args)
}
