use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};

use crate::app_paths::AppPaths;
use crate::collector;
use crate::config::Config;
use crate::db::Database;
use crate::deployment::{
    DeploymentRequest, DeploymentStateStore, PublisherKey, RemoteExecutor, SignedArtifactManifest,
    SshRemoteExecutor,
};
use crate::enrollment::{HostKeyObservation, HostKeyStatus, KnownHostStore};
use crate::hub::ListenerTrustMode;
use crate::importers::{self, ImportOptions};
use crate::listener::{ListenerPlan, PublicTrustConfig};
use crate::loop_upgrade;
use crate::pricing;
use crate::remote;
use crate::server;
use crate::ssh::{canonical_known_hosts_line, host_key_fingerprint, CanonicalSshTarget};

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
    /// Deploy a signed Hub and its local Collector over SSH.
    Deploy(DeployCommand),
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

    /// Run the authenticated Hub listener rather than loopback local serve.
    #[arg(long)]
    pub hub: bool,

    /// Hub listener trust mode. Tailscale is private-by-default.
    #[arg(long, default_value = "tailscale")]
    pub listener: String,
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
    /// Run the outbound-only Collector daemon (startup, watcher, delivery, and command poller).
    Run(CollectorRunArgs),
    /// Reconcile local harness sources and queue one durable outbound batch.
    Reconcile(CollectorReconcileArgs),
    /// Print metadata-only Collector diagnostics.
    Diagnostics(CollectorDiagnosticsArgs),
    /// Explicitly recover one terminal/dead-lettered outbox batch.
    Recover(CollectorRecoverArgs),
}

#[derive(Debug, Args)]
pub struct CollectorRunArgs {
    /// Perform startup reconciliation and one delivery pass, then exit.
    #[arg(long)]
    pub once: bool,
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
pub struct CollectorRecoverArgs {
    #[arg(long)]
    pub batch_id: String,
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
pub struct DeployCommand {
    #[command(subcommand)]
    pub command: DeploySubcommand,
}

#[derive(Debug, Subcommand)]
pub enum DeploySubcommand {
    Hub(DeployHubArgs),
}

#[derive(Debug, Args)]
pub struct DeployHubArgs {
    /// SSH alias or user@host target. It is passed as one fixed SSH argument.
    pub ssh_target: String,
    /// Print the typed plan and do not probe or mutate the remote.
    #[arg(long)]
    pub plan: bool,
    /// Apply only after local signed-artifact verification and remote probing.
    #[arg(long)]
    pub apply: bool,
    /// Render the plan/receipt as machine-readable JSON.
    #[arg(long)]
    pub json: bool,
    /// Signed manifest JSON. Required for --apply.
    #[arg(long)]
    pub manifest: Option<PathBuf>,
    /// Directory containing the manifest-selected artifact files.
    #[arg(long)]
    pub artifact_dir: Option<PathBuf>,
    /// Ed25519 public key file (raw 32 bytes or hexadecimal).
    #[arg(long)]
    pub public_key: Option<PathBuf>,
    /// Pinned publisher key ID from the release allowlist.
    #[arg(long)]
    pub publisher_key_id: Option<String>,
    /// Pinned SHA-256 fingerprint for the publisher public key.
    #[arg(long)]
    pub publisher_fingerprint: Option<String>,
    /// Hash printed by the concrete planning probe and explicitly reviewed by the operator.
    #[arg(long)]
    pub approved_plan_hash: Option<String>,
    /// Exact observed managed host-key fingerprint required on first use.
    #[arg(long)]
    pub confirm_host_fingerprint: Option<String>,
    /// Optional local SQLite seed, transferred through SSH stdin.
    #[arg(long)]
    pub db_seed: Option<PathBuf>,
    /// Listener mode. Tailscale Serve is the secure default.
    #[arg(long, default_value = "tailscale")]
    pub listener: String,
    /// Public reverse-proxy CIDR(s); public mode still uses fallback admin auth.
    #[arg(long)]
    pub trusted_proxy_cidr: Vec<String>,
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
        Some(Command::Serve(args)) => serve(&paths, args, &config),
        Some(Command::Doctor(args)) => doctor(&paths, &config, args),
        Some(Command::Collector(args)) => collector_command(&paths, &config, args),
        Some(Command::Remote(args)) => remote::run(&paths, &mut config, args),
        Some(Command::Deploy(args)) => deploy_command(&paths, &config, args),
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

fn serve(paths: &AppPaths, args: ServeArgs, config: &Config) -> Result<()> {
    let db = Database::open(&paths.db_path)?;
    db.migrate()?;
    pricing::seed_bundled_pricing(&db)?;
    if args.hub {
        let trust_mode = match args.listener.trim().to_ascii_lowercase().as_str() {
            "tailscale" | "tailscale-serve" => ListenerTrustMode::PrivateTailscale,
            "public" | "public-https"
                if config
                    .hub
                    .listener
                    .public
                    .as_ref()
                    .and_then(|public| public.trusted_proxy.as_ref())
                    .is_some() =>
            {
                ListenerTrustMode::TrustedProxy
            }
            "public" | "public-https" => ListenerTrustMode::Public,
            other => anyhow::bail!("unsupported Hub listener mode {other}"),
        };
        return crate::hub::serve(
            paths.db_path.clone(),
            args.host,
            args.port,
            trust_mode,
            &config.hub,
        );
    }
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
        CollectorSubcommand::Run(run_args) => {
            if run_args.once {
                let report = runtime.reconcile_startup(chrono::Utc::now())?;
                let hub_url = config
                    .collector
                    .hub_url
                    .as_deref()
                    .context("collector.hub_url is required for `collector run --once`")?;
                let mut transport = collector::CollectorHttpTransport::new(hub_url)?;
                let delivery = runtime.deliver_pending(&mut transport, chrono::Utc::now())?;
                println!(
                    "collector run once: batch={} acknowledged={} pending={} terminal={}",
                    report.batch_id.as_deref().unwrap_or("none"),
                    delivery.acknowledged,
                    delivery.pending,
                    delivery.terminal
                );
            } else {
                collector::run_daemon(paths, config)?;
            }
        }
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
                if report.terminal_outbox > 0 {
                    println!(
                        "collector terminal outbox batches={}",
                        report.terminal_outbox
                    );
                }
            }
        }
        CollectorSubcommand::Recover(recover_args) => {
            let recovered =
                runtime.recover_outbox_batch(&recover_args.batch_id, chrono::Utc::now())?;
            if recover_args.json {
                println!(
                    "{}",
                    serde_json::json!({"batch_id": recover_args.batch_id, "recovered": recovered})
                );
            } else if recovered {
                println!("collector recovered outbox batch {}", recover_args.batch_id);
            } else {
                println!(
                    "collector outbox batch {} was not terminal",
                    recover_args.batch_id
                );
            }
        }
    }
    Ok(())
}

fn deploy_command(paths: &AppPaths, config: &Config, args: DeployCommand) -> Result<()> {
    match args.command {
        DeploySubcommand::Hub(args) => deploy_hub(paths, config, args),
    }
}

fn deploy_hub(paths: &AppPaths, config: &Config, args: DeployHubArgs) -> Result<()> {
    let listener = match args.listener.trim().to_ascii_lowercase().as_str() {
        "tailscale" | "tailscale-serve" => {
            ListenerPlan::tailscale_default(crate::deployment::DEFAULT_HUB_PORT)
        }
        "public" | "public-https" => ListenerPlan::public_https(
            crate::deployment::DEFAULT_HUB_PORT,
            PublicTrustConfig {
                trusted_proxy: if args.trusted_proxy_cidr.is_empty() {
                    None
                } else {
                    Some(crate::listener::PublicTrustedProxy {
                        identity_header: "x-dirtydash-identity".to_string(),
                        provenance_header: "x-dirtydash-proxy-provenance".to_string(),
                        provenance_value: "proxy-verified".to_string(),
                        source_cidrs: args.trusted_proxy_cidr.clone(),
                    })
                },
                ..PublicTrustConfig::default()
            },
        )?,
        other => anyhow::bail!("unsupported listener mode {other}; use tailscale or public"),
    };
    if args.plan && args.apply {
        anyhow::bail!("--plan and --apply are mutually exclusive");
    }
    let state_path = paths
        .config_path
        .parent()
        .context("config path has no parent")?
        .join("deployment-checkpoint.json");

    // Keep the old no-input inspection command useful as a local shape
    // preview. It is intentionally not persisted or eligible for apply;
    // concrete plans require the signed artifact inputs below.
    if args.plan
        && (args.manifest.is_none() || args.artifact_dir.is_none() || args.public_key.is_none())
    {
        // Even a shape preview must not make an untrusted publisher look like
        // an eligible deployment.  The anchor is read from durable config;
        // flags alone are never sufficient.
        configured_publisher_anchor(config, &args)?;
        let plan = crate::deployment::DeploymentPlan::skeleton(
            args.ssh_target,
            env!("CARGO_PKG_VERSION"),
            listener,
            args.db_seed.is_some(),
        )?;
        if args.json {
            println!("{}", plan.to_json()?);
        } else {
            println!("{}", plan.to_json()?);
            println!("shape preview only: provide signed manifest, artifact directory, and pinned publisher to run the concrete probe");
        }
        return Ok(());
    }

    // Planning is a concrete read-only probe.  Artifact evidence is required
    // so the persisted plan cannot be approved for an unknown release.
    if args.plan {
        verify_publisher_inputs(&args, config)?;
        let known_hosts = managed_known_hosts(paths)?;
        let canonical = CanonicalSshTarget::resolve(&args.ssh_target)?;
        confirm_managed_first_use(
            &canonical,
            &known_hosts,
            args.confirm_host_fingerprint.as_deref(),
        )?;
        let mut executor =
            SshRemoteExecutor::from_canonical_target(canonical, known_hosts.clone())?;
        let platform = executor.detect()?.platform;
        let inputs = deployment_inputs(&args, config, false, Some(platform))?;
        let request = DeploymentRequest {
            target: args.ssh_target,
            release: inputs.signed.manifest.release.clone(),
            listener,
            database_seed: inputs.seed,
            approved_plan_hash: None,
        };
        let mut runner = crate::deployment::DeploymentRunner::new(executor)
            .with_state_store(DeploymentStateStore::new(state_path));
        let plan = runner.probe(&request, Some(&inputs.artifact))?;
        if args.json {
            println!("{}", plan.to_json()?);
        } else {
            println!("{}", plan.to_json()?);
            println!(
                "review plan hash {} and pass it with --approved-plan-hash --apply",
                plan.plan_hash
            );
        }
        return Ok(());
    }

    if !args.apply {
        anyhow::bail!("pass --plan for a concrete probe or --apply with an approved plan hash");
    }

    verify_publisher_inputs(&args, config)?;
    let known_hosts = managed_known_hosts(paths)?;
    let canonical = CanonicalSshTarget::resolve(&args.ssh_target)?;
    confirm_managed_first_use(
        &canonical,
        &known_hosts,
        args.confirm_host_fingerprint.as_deref(),
    )?;
    let mut executor = SshRemoteExecutor::from_canonical_target(canonical, known_hosts.clone())?;
    let platform = executor.detect()?.platform;
    let inputs = deployment_inputs(&args, config, true, Some(platform))?;
    let request = DeploymentRequest {
        target: args.ssh_target,
        release: inputs.signed.manifest.release.clone(),
        listener,
        database_seed: inputs.seed,
        approved_plan_hash: args.approved_plan_hash,
    };
    let mut runner = crate::deployment::DeploymentRunner::new(executor)
        .with_state_store(DeploymentStateStore::new(state_path));
    let receipt = runner.apply(&request, &inputs.artifact)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&receipt)?);
    } else {
        println!("deployment {}: {}", receipt.release, receipt.status);
        if receipt.tailscale_state == crate::listener::TailscaleServeState::ConsentRequired {
            println!("Tailscale Serve requires explicit consent; rerun --apply after approving it");
        }
    }
    Ok(())
}

struct DeploymentInputs {
    signed: SignedArtifactManifest,
    artifact: crate::deployment::VerifiedArtifact,
    seed: Option<Vec<u8>>,
}

fn configured_publisher_anchor<'a>(
    config: &'a Config,
    args: &DeployHubArgs,
) -> Result<(&'a str, &'a str)> {
    let key_id = config
        .hub
        .allowed_publisher_key_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .context(
            "deployment requires a durable configured publisher key ID; CLI flags cannot establish trust",
        )?;
    let fingerprint = config
        .hub
        .allowed_publisher_fingerprint
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .context(
            "deployment requires a durable configured publisher fingerprint; CLI flags cannot establish trust",
        )?;
    if let Some(value) = args.publisher_key_id.as_deref() {
        if value != key_id {
            anyhow::bail!(
                "--publisher-key-id does not match the configured publisher trust anchor"
            );
        }
    }
    if let Some(value) = args.publisher_fingerprint.as_deref() {
        if !value.eq_ignore_ascii_case(fingerprint) {
            anyhow::bail!(
                "--publisher-fingerprint does not match the configured publisher trust anchor"
            );
        }
    }
    Ok((key_id, fingerprint))
}

fn verify_publisher_inputs(args: &DeployHubArgs, config: &Config) -> Result<()> {
    let manifest_path = args
        .manifest
        .as_deref()
        .context("--manifest is required for deployment planning/apply")?;
    let artifact_dir = args
        .artifact_dir
        .as_deref()
        .context("--artifact-dir is required for deployment planning/apply")?;
    let public_key_path = args
        .public_key
        .as_deref()
        .context("--public-key is required for deployment planning/apply")?;
    let (key_id, fingerprint) = configured_publisher_anchor(config, args)?;
    let signed: SignedArtifactManifest = serde_json::from_slice(
        &fs::read(manifest_path)
            .with_context(|| format!("reading signed manifest {}", manifest_path.display()))?,
    )
    .context("parsing signed artifact manifest")?;
    let public_key = read_public_key(public_key_path)?;
    let publisher = PublisherKey::new(key_id, fingerprint, &public_key)?;
    let verified_manifest = signed.verify_with_publisher(&publisher)?;
    for descriptor in &verified_manifest.manifest().artifacts {
        let bytes = fs::read(artifact_dir.join(&descriptor.file)).with_context(|| {
            format!(
                "reading signed artifact {}",
                artifact_dir.join(&descriptor.file).display()
            )
        })?;
        verified_manifest.verify_artifact(descriptor.platform, bytes)?;
    }
    Ok(())
}

fn deployment_inputs(
    args: &DeployHubArgs,
    config: &Config,
    applying: bool,
    platform: Option<crate::deployment::TargetPlatform>,
) -> Result<DeploymentInputs> {
    let manifest_path = args
        .manifest
        .as_deref()
        .context("--manifest is required for deployment planning/apply")?;
    let artifact_dir = args
        .artifact_dir
        .as_deref()
        .context("--artifact-dir is required for deployment planning/apply")?;
    let public_key_path = args
        .public_key
        .as_deref()
        .context("--public-key is required for deployment planning/apply")?;

    let (key_id, fingerprint) = configured_publisher_anchor(config, args)?;
    let signed: SignedArtifactManifest = serde_json::from_slice(
        &fs::read(manifest_path)
            .with_context(|| format!("reading signed manifest {}", manifest_path.display()))?,
    )
    .context("parsing signed artifact manifest")?;
    let public_key = read_public_key(public_key_path)?;
    let publisher = PublisherKey::new(key_id, fingerprint, &public_key)?;
    let verified_manifest = signed.verify_with_publisher(&publisher)?;
    // Selection is completed after the remote probe by the runner.  Local
    // artifact verification still uses every declared digest/size invariant.
    let descriptor = match platform {
        Some(platform) => verified_manifest.select(platform)?,
        None if verified_manifest.manifest().artifacts.len() == 1 => {
            &verified_manifest.manifest().artifacts[0]
        }
        None => anyhow::bail!("a concrete target platform is required to select an artifact"),
    };
    let artifact_path = artifact_dir.join(&descriptor.file);
    let bytes = fs::read(&artifact_path)
        .with_context(|| format!("reading signed artifact {}", artifact_path.display()))?;
    let platform = descriptor.platform;
    let artifact = verified_manifest.verify_artifact(platform, bytes)?;
    let seed = args
        .db_seed
        .as_deref()
        .map(|path| -> Result<Vec<u8>> {
            let bytes = fs::read(path)
                .with_context(|| format!("reading SQLite seed {}", path.display()))?;
            crate::deployment::validate_sqlite_header(&bytes)
                .with_context(|| format!("validating SQLite seed {}", path.display()))?;
            Ok(bytes)
        })
        .transpose()?;
    if applying && args.approved_plan_hash.is_none() {
        anyhow::bail!("--approved-plan-hash is required for --apply");
    }
    Ok(DeploymentInputs {
        signed,
        artifact,
        seed,
    })
}

fn managed_known_hosts(paths: &AppPaths) -> Result<PathBuf> {
    Ok(paths
        .config_path
        .parent()
        .context("config path has no parent")?
        .join("deployment-known_hosts"))
}

fn confirm_managed_first_use(
    target: &CanonicalSshTarget,
    known_hosts_path: &Path,
    confirmation: Option<&str>,
) -> Result<()> {
    let output = ProcessCommand::new("ssh-keyscan")
        .args(target.keyscan_args())
        .output()
        .context("observing remote SSH host key")?;
    if !output.status.success() || output.stdout.is_empty() {
        anyhow::bail!("SSH host-key observation failed");
    }
    let line = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    let known_hosts_line = canonical_known_hosts_line(target, &line)?;
    let observation =
        HostKeyObservation::new(host_key_fingerprint(&known_hosts_line)?, known_hosts_line)?;
    let store = KnownHostStore::new(known_hosts_path);
    match store.status(&target.host_key_name(), &observation.fingerprint)? {
        HostKeyStatus::Matching => Ok(()),
        HostKeyStatus::Changed => anyhow::bail!(
            "managed host key changed; refusing deployment without destructive trust reset"
        ),
        HostKeyStatus::Unknown => {
            if confirmation != Some(observation.fingerprint.as_str()) {
                anyhow::bail!(
                    "first use requires --confirm-host-fingerprint {}",
                    observation.fingerprint
                );
            }
            store.confirm_unknown(&target.host_key_name(), &observation)
        }
    }
}

fn read_public_key(path: &std::path::Path) -> Result<Vec<u8>> {
    let bytes =
        fs::read(path).with_context(|| format!("reading Ed25519 public key {}", path.display()))?;
    if bytes.len() == 32 {
        return Ok(bytes);
    }
    let text =
        String::from_utf8(bytes).context("public key is neither raw bytes nor hexadecimal")?;
    let text = text.trim();
    if text.len() != 64 || !text.chars().all(|character| character.is_ascii_hexdigit()) {
        anyhow::bail!("public key must contain exactly 32 raw bytes or 64 hexadecimal characters");
    }
    Ok(hex::decode(text)?)
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
        hub: false,
        listener: "tailscale".to_string(),
    };
    server::serve(paths.db_path, args)
}
