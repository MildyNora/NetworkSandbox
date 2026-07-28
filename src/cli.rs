use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::{Args, Parser, Subcommand};

use crate::apply::{
    apply_plan, build_guarded_trial_plan, build_plan, copy_entry, rollback_transaction,
    verify_route_changes,
};
use crate::connectivity::{capture_baseline, describe_capture_support, validate_capabilities};
use crate::diff::{inspect_origin, render_change_diff, scan_changes, validate_relative_path};
use crate::model::{
    Capability, Direction, Environment, EnvironmentPolicy, EnvironmentStatus, FailurePolicy,
    ProbeSpec, RouteCandidate, ValidationState,
};
use crate::platform;
use crate::store::{Store, validate_name};

#[derive(Debug, Parser)]
#[command(
    name = "netsandbox",
    version,
    about = "Safely rehearse and transactionally apply system and network changes"
)]
pub struct Cli {
    #[arg(long, global = true, env = "NETSANDBOX_STATE_DIR")]
    state_dir: Option<PathBuf>,

    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a named differential environment.
    Create(CreateArgs),
    /// Enter an interactive shell in an environment.
    Enter(EnterArgs),
    /// Execute one command in an environment.
    Exec(ExecArgs),
    /// List environments.
    List,
    /// Show a concise environment summary.
    Status { name: String },
    /// Show complete environment metadata.
    Inspect { name: String },
    /// List changed paths.
    Changes { name: String },
    /// Render filesystem differences.
    Diff { name: String, path: Option<PathBuf> },
    /// Copy a host file into the differential layer, replace it, or stage its deletion.
    Stage(StageArgs),
    /// Remove one or all experimental changes.
    Reset(ResetArgs),
    /// Capture or inspect the original connection baseline.
    Baseline {
        #[command(subcommand)]
        command: BaselineCommand,
    },
    /// Show the connection capability comparison.
    Connections { name: String },
    /// Validate the baseline from the current network view.
    Check(CheckArgs),
    /// Continuously repeat connectivity validation.
    Watch(WatchArgs),
    /// Add or classify end-to-end circuit checks.
    Circuit {
        #[command(subcommand)]
        command: CircuitCommand,
    },
    /// Manage macOS VM isolation and typed route candidates.
    Mac {
        #[command(subcommand)]
        command: MacCommand,
    },
    /// Preview whether an environment can be applied.
    Plan { name: String },
    /// Transactionally apply the recorded difference.
    Apply(ApplyArgs),
    /// Delete an unapplied environment and its differences.
    Discard(ConfirmNameArgs),
    /// Remove a finished or empty environment.
    Remove(ConfirmNameArgs),
    /// List host application transactions.
    History,
    /// Restore the host from an application transaction.
    Rollback(RollbackArgs),
    /// Check whether the host supports the isolation backend.
    Doctor,
    #[command(name = "__sandbox-init", hide = true)]
    SandboxInit {
        name: String,
        #[arg(last = true, required = true)]
        command: Vec<OsString>,
    },
    #[command(name = "__probe", hide = true)]
    Probe {
        name: String,
        #[arg(long, default_value_t = 3)]
        timeout: u64,
    },
    #[command(name = "__rollback-guard", hide = true)]
    RollbackGuard {
        transaction: String,
        #[arg(long, default_value_t = 15)]
        timeout: u64,
    },
}

#[derive(Debug, Args)]
struct CreateArgs {
    name: String,
    #[arg(long)]
    description: Option<String>,
    #[arg(long, default_value = "/")]
    base: PathBuf,
    #[arg(long, default_value = "block-apply")]
    on_failure: FailurePolicy,
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    auto_rollback: bool,
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    capture_connections: bool,
}

#[derive(Debug, Args)]
struct EnterArgs {
    name: String,
    #[arg(long)]
    shell: Option<OsString>,
}

#[derive(Debug, Args)]
struct ExecArgs {
    name: String,
    #[arg(last = true, required = true)]
    command: Vec<OsString>,
}

#[derive(Debug, Args)]
struct ResetArgs {
    name: String,
    paths: Vec<PathBuf>,
    #[arg(long)]
    all: bool,
    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Args)]
struct StageArgs {
    name: String,
    /// Host path represented by this differential change.
    path: PathBuf,
    /// Candidate file to use instead of copying the current host file.
    #[arg(long)]
    from: Option<PathBuf>,
    /// Stage deletion of the host path.
    #[arg(long)]
    delete: bool,
}

#[derive(Debug, Subcommand)]
enum BaselineCommand {
    Show {
        name: String,
    },
    Refresh {
        name: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
enum CircuitCommand {
    /// Add a safe application-level command that must succeed inside the sandbox.
    Add {
        environment: String,
        name: String,
        #[arg(last = true, required = true)]
        command: Vec<OsString>,
    },
    /// Add a direct TCP reachability canary.
    AddTcp {
        environment: String,
        name: String,
        endpoint: String,
        /// Bind the probe socket to this interface without changing host routes.
        #[arg(long)]
        interface: Option<String>,
    },
    /// Make an observed circuit non-blocking.
    Ignore { environment: String, id: String },
    /// Make a circuit required again.
    Require { environment: String, id: String },
    /// Remove a manually added circuit.
    Remove { environment: String, id: String },
}

#[derive(Debug, Subcommand)]
enum MacCommand {
    /// Create a lightweight isolated environment from a Linux container image.
    LinuxCreate {
        environment: String,
        image: String,
        /// Linux netsandbox binary matching the image architecture and libc.
        #[arg(long, env = "NETSANDBOX_LINUX_GUEST_BIN")]
        guest_binary: Option<PathBuf>,
    },
    /// Show the lightweight Linux image runtime status.
    LinuxStatus { environment: String },
    /// Capture and track one regular file from the pristine Linux image.
    LinuxTrack { environment: String, path: PathBuf },
    /// Export every tracked Linux image file into the differential layer.
    LinuxSync { environment: String },
    /// Show all container-layer changes and whether each is tracked.
    LinuxDiff { environment: String },
    /// Recreate a Linux image environment from its original image.
    LinuxReset {
        environment: String,
        #[arg(long)]
        yes: bool,
    },
    /// Commit an allowlisted, validated Linux environment to a new immutable image.
    LinuxCommit {
        environment: String,
        output_image: String,
        #[arg(long)]
        yes: bool,
    },
    /// Remove the image created by linux-commit after verifying its recorded ID.
    LinuxRollback {
        environment: String,
        #[arg(long)]
        yes: bool,
    },
    /// Clone a prepared Lima macOS base VM for isolated enter/exec.
    VmClone {
        environment: String,
        base_instance: String,
    },
    /// Attach an existing isolated Lima macOS VM to an environment.
    VmAttach {
        environment: String,
        instance: String,
    },
    /// Show the attached macOS VM status.
    VmStatus { environment: String },
    /// Track one guest path and export it into the differential layer after commands.
    VmTrack { environment: String, path: PathBuf },
    /// Export every tracked guest path into the differential layer now.
    VmSync { environment: String },
    /// Recreate a managed VM clone from its prepared base and clear its tracked exports.
    VmReset {
        environment: String,
        #[arg(long)]
        yes: bool,
    },
    /// Show the route macOS currently selects for a destination.
    RouteShow { destination: String },
    /// Stage an interface-scoped route candidate and its TCP checks.
    RoutePreview {
        environment: String,
        destination: String,
        #[arg(long)]
        interface: String,
        /// Optional next-hop gateway. Without this, install a direct interface route.
        #[arg(long)]
        gateway: Option<String>,
        #[arg(long = "port", required = true)]
        ports: Vec<u16>,
    },
    /// List staged route candidates.
    RouteList { environment: String },
    /// Test candidates and revalidate every protected baseline circuit.
    Test {
        environment: String,
        #[arg(long, default_value_t = 5)]
        timeout: u64,
    },
    /// Associate a non-mutating application-level canary with one route candidate.
    RouteCanary {
        environment: String,
        candidate: String,
        name: String,
        #[arg(last = true, required = true)]
        command: Vec<OsString>,
    },
    /// Remove an unapplied staged candidate.
    RouteRemove {
        environment: String,
        candidate: String,
    },
}

#[derive(Debug, Args)]
struct CheckArgs {
    name: String,
    #[arg(long, default_value_t = 3)]
    timeout: u64,
    /// Run in the current namespace. Intended for an already-entered sandbox.
    #[arg(long, hide = true)]
    current_namespace: bool,
}

#[derive(Debug, Args)]
struct WatchArgs {
    name: String,
    #[arg(long, default_value_t = 5)]
    interval: u64,
    /// Stop after this many checks. Zero means until interrupted.
    #[arg(long, default_value_t = 0)]
    count: u32,
    #[arg(long, default_value_t = 3)]
    timeout: u64,
}

#[derive(Debug, Args)]
struct ApplyArgs {
    name: String,
    #[arg(long)]
    dry_run: bool,
    /// Temporarily install typed macOS host routes under the rollback watchdog,
    /// then validate route-associated circuits before committing.
    #[arg(long)]
    trial: bool,
    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Args)]
struct ConfirmNameArgs {
    name: String,
    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Args)]
struct RollbackArgs {
    transaction: String,
    #[arg(long)]
    yes: bool,
}

pub fn run() -> Result<i32> {
    let cli = Cli::parse();
    dispatch(cli)
}

fn dispatch(cli: Cli) -> Result<i32> {
    if let Command::RollbackGuard {
        transaction,
        timeout,
    } = &cli.command
    {
        let store = Store::open_unlocked(cli.state_dir)?;
        return rollback_guard(&store, transaction, *timeout);
    }
    if let Command::SandboxInit { name, command } = &cli.command {
        let store = Store::open_unlocked(cli.state_dir)?;
        let environment = store.load_environment(name)?;
        return platform_sandbox_init(&environment, &store, command);
    }
    if let Command::Probe { timeout, .. } = &cli.command
        && let Some(control) = std::env::var_os("NETSANDBOX_CONTROL")
    {
        return control_probe(Path::new(&control), *timeout, cli.json);
    }
    if let Command::Check(args) = &cli.command
        && std::env::var("NETSANDBOX_ACTIVE").ok().as_deref() == Some(args.name.as_str())
        && let Some(control) = std::env::var_os("NETSANDBOX_CONTROL")
    {
        return control_probe(Path::new(&control), args.timeout, cli.json);
    }

    let store = Store::open(cli.state_dir.clone())?;
    match cli.command {
        Command::Create(args) => create(&store, args, cli.json),
        Command::Enter(args) => enter(store, args),
        Command::Exec(args) => execute(store, args),
        Command::List => list(&store, cli.json),
        Command::Status { name } => status(&store, &name, cli.json),
        Command::Inspect { name } => inspect(&store, &name),
        Command::Changes { name } => changes(&store, &name, cli.json),
        Command::Diff { name, path } => diff(&store, &name, path.as_deref()),
        Command::Stage(args) => stage(&store, args, cli.json),
        Command::Reset(args) => reset(&store, args),
        Command::Baseline { command } => baseline(&store, command, cli.json),
        Command::Connections { name } => connections(&store, &name, cli.json),
        Command::Check(args) => check(store, args, cli.json),
        Command::Watch(args) => watch(store, args, cli.json),
        Command::Circuit { command } => circuit(&store, command),
        Command::Mac { command } => mac(&store, command, cli.json),
        Command::Plan { name } => plan(&store, &name, cli.json),
        Command::Apply(args) => apply(&store, args, cli.json),
        Command::Discard(args) => discard(&store, args),
        Command::Remove(args) => remove(&store, args),
        Command::History => history(&store, cli.json),
        Command::Rollback(args) => rollback(&store, args),
        Command::Doctor => doctor(cli.json),
        Command::Probe { name, timeout } => probe(&store, &name, timeout, cli.json),
        Command::RollbackGuard { .. } => unreachable!(),
        Command::SandboxInit { .. } => unreachable!(),
    }
}

fn create(store: &Store, args: CreateArgs, json: bool) -> Result<i32> {
    validate_name(&args.name)?;
    let base = args
        .base
        .canonicalize()
        .with_context(|| format!("base root {} does not exist", args.base.display()))?;
    let baseline = if args.capture_connections {
        capture_baseline()?
    } else {
        Vec::new()
    };
    let policy = EnvironmentPolicy {
        on_failure: args.on_failure,
        auto_rollback: args.auto_rollback,
    };
    let environment = Environment::new(args.name, args.description, base, policy, baseline);
    store.create_environment(&environment)?;
    if json {
        print_json(&environment)?;
    } else {
        println!("Created environment '{}'.", environment.name);
        println!(
            "Captured {} active connection capabilities.",
            environment.baseline.len()
        );
        if !cfg!(target_os = "linux") {
            println!("Note: {}", describe_capture_support());
        }
        if cfg!(target_os = "linux") {
            println!("Enter with: netsandbox enter {}", environment.name);
        } else if cfg!(target_os = "macos") {
            println!(
                "Stage a route candidate with: netsandbox mac route-preview {} DESTINATION --interface INTERFACE --port PORT",
                environment.name
            );
            println!(
                "Stage files, then use dependency-free native execution: netsandbox exec {} -- COMMAND ARGUMENT...",
                environment.name
            );
            println!(
                "Legacy guest backends remain optional: netsandbox mac vm-clone {} BASE_INSTANCE",
                environment.name
            );
        }
    }
    Ok(0)
}

fn enter(store: Store, args: EnterArgs) -> Result<i32> {
    let mut environment = store.load_environment(&args.name)?;
    ensure_enterable(&environment)?;
    let state_root = store.root().to_path_buf();
    environment.status = EnvironmentStatus::Running;
    environment.updated_at = Utc::now();
    store.save_environment(&environment)?;
    prepare_control(&store, &environment)?;
    let shell = args
        .shell
        .unwrap_or_else(|| platform::default_shell(&environment));
    let command = vec![shell, OsString::from("-i")];
    drop(store);
    let result = platform::run_in_environment(&environment, &state_root, &command);
    let store = Store::open(Some(state_root))?;
    let mut current = store.load_environment(&environment.name)?;
    import_probe_result(&store, &mut current)?;
    current.status = if current
        .baseline
        .iter()
        .any(|capability| capability.required && capability.validation == ValidationState::Lost)
    {
        EnvironmentStatus::ValidationFailed
    } else {
        EnvironmentStatus::Ready
    };
    current.updated_at = Utc::now();
    store.save_environment(&current)?;
    result
}

fn execute(store: Store, args: ExecArgs) -> Result<i32> {
    let mut environment = store.load_environment(&args.name)?;
    ensure_enterable(&environment)?;
    let state_root = store.root().to_path_buf();
    environment.status = EnvironmentStatus::Running;
    environment.updated_at = Utc::now();
    store.save_environment(&environment)?;
    prepare_control(&store, &environment)?;
    drop(store);
    let result = platform::run_in_environment(&environment, &state_root, &args.command);
    let store = Store::open(Some(state_root))?;
    let mut current = store.load_environment(&environment.name)?;
    import_probe_result(&store, &mut current)?;
    current.status = if result.as_ref().is_ok_and(|code| *code == 0) {
        EnvironmentStatus::Ready
    } else {
        EnvironmentStatus::ValidationFailed
    };
    current.updated_at = Utc::now();
    store.save_environment(&current)?;
    result
}

fn list(store: &Store, json: bool) -> Result<i32> {
    let environments = store.list_environments()?;
    if json {
        print_json(&environments)?;
    } else if environments.is_empty() {
        println!("No environments.");
    } else {
        println!(
            "{:<24} {:<20} {:<9} CONNECTIONS",
            "NAME", "STATE", "CHANGES"
        );
        for mut environment in environments {
            let upper = store.upper_dir(&environment.name);
            let count = scan_changes(&mut environment, &upper)?.len();
            println!(
                "{:<24} {:<20} {:<9} {}",
                environment.name,
                environment.status,
                count,
                environment.baseline.len()
            );
        }
    }
    Ok(0)
}

fn status(store: &Store, name: &str, json: bool) -> Result<i32> {
    let mut environment = store.load_environment(name)?;
    let changes = scan_changes(&mut environment, &store.upper_dir(name))?;
    store.save_environment(&environment)?;
    if json {
        print_json(&serde_json::json!({
            "environment": environment,
            "change_count": changes.len(),
        }))?;
    } else {
        println!("Environment: {}", environment.name);
        println!("State:       {}", environment.status);
        println!("Changes:     {}", changes.len());
        println!("Connections: {}", environment.baseline.len());
        if let Some(runtime) = platform::runtime_description(&environment) {
            println!("Runtime:     {runtime}");
            println!("Tracked:     {}", environment.tracked_paths.len());
        }
        let lost = environment
            .baseline
            .iter()
            .filter(|capability| capability.validation == ValidationState::Lost)
            .count();
        println!("Lost:        {lost}");
    }
    Ok(0)
}

fn inspect(store: &Store, name: &str) -> Result<i32> {
    print_json(&store.load_environment(name)?)?;
    Ok(0)
}

fn changes(store: &Store, name: &str, json: bool) -> Result<i32> {
    let mut environment = store.load_environment(name)?;
    let changes = scan_changes(&mut environment, &store.upper_dir(name))?;
    store.save_environment(&environment)?;
    if json {
        print_json(&changes)?;
    } else if changes.is_empty() {
        println!("No filesystem changes.");
    } else {
        for change in changes {
            println!("{:<13} {}", change.kind, change.path.display());
        }
    }
    Ok(0)
}

fn diff(store: &Store, name: &str, selected: Option<&Path>) -> Result<i32> {
    let mut environment = store.load_environment(name)?;
    let all_changes = scan_changes(&mut environment, &store.upper_dir(name))?;
    store.save_environment(&environment)?;
    let selected = selected.map(normalize_user_path).transpose()?;
    let mut rendered = false;
    for change in &all_changes {
        if selected
            .as_ref()
            .is_some_and(|path| path.as_path() != change.path)
        {
            continue;
        }
        print!(
            "{}",
            render_change_diff(&environment, &store.upper_dir(name), change)?
        );
        rendered = true;
    }
    if !rendered {
        println!("No matching filesystem changes.");
    }
    Ok(0)
}

fn stage(store: &Store, args: StageArgs, json: bool) -> Result<i32> {
    if args.delete && args.from.is_some() {
        bail!("--delete and --from cannot be used together");
    }
    let mut environment = store.load_environment(&args.name)?;
    ensure_enterable(&environment)?;
    let relative = normalize_user_path(&args.path)?;
    let target = environment.base_root.join(&relative);
    let origin = inspect_origin(&target)?;
    environment
        .origins
        .entry(relative.clone())
        .or_insert(origin.clone());
    let candidate = store.upper_dir(&args.name).join(&relative);

    if args.delete {
        if !origin.existed {
            bail!(
                "cannot stage deletion because {} does not exist",
                target.display()
            );
        }
        remove_upper_path(&candidate)?;
        if !environment.deleted_paths.contains(&relative) {
            environment.deleted_paths.push(relative.clone());
        }
    } else {
        let source = args.from.as_deref().unwrap_or(&target);
        let metadata = fs::symlink_metadata(source)
            .with_context(|| format!("inspect stage source {}", source.display()))?;
        if metadata.file_type().is_dir() {
            bail!("staging directories is not supported; stage the changed files individually");
        }
        copy_entry(source, &candidate)?;
        environment
            .deleted_paths
            .retain(|deleted| deleted != &relative);
    }

    environment.updated_at = Utc::now();
    store.save_environment(&environment)?;
    if json {
        print_json(&serde_json::json!({
            "environment": environment.name,
            "host_path": target,
            "candidate_path": if args.delete { None } else { Some(candidate) },
            "deleted": args.delete,
        }))?;
    } else if args.delete {
        println!("Staged deletion of {}.", target.display());
    } else {
        println!("Staged {}.", target.display());
        println!("Candidate file: {}", candidate.display());
        println!(
            "Edit the candidate, then run: netsandbox diff {}",
            args.name
        );
    }
    Ok(0)
}

fn reset(store: &Store, args: ResetArgs) -> Result<i32> {
    if (args.all && !args.paths.is_empty()) || (!args.all && args.paths.is_empty()) {
        bail!("provide paths, or use --all");
    }
    if args.all && !args.yes {
        bail!("resetting all changes requires --yes");
    }
    let mut environment = store.load_environment(&args.name)?;
    let upper = store.upper_dir(&args.name);
    let paths = if args.all {
        scan_changes(&mut environment, &upper)?
            .into_iter()
            .map(|change| change.path)
            .collect()
    } else {
        args.paths
            .iter()
            .map(|path| normalize_user_path(path))
            .collect::<Result<Vec<_>>>()?
    };
    for path in &paths {
        remove_upper_path(&upper.join(path))?;
        environment.deleted_paths.retain(|deleted| deleted != path);
        environment.origins.remove(path);
    }
    environment.updated_at = Utc::now();
    store.save_environment(&environment)?;
    println!("Reset {} path(s) in '{}'.", paths.len(), args.name);
    Ok(0)
}

fn baseline(store: &Store, command: BaselineCommand, json: bool) -> Result<i32> {
    match command {
        BaselineCommand::Show { name } => connections(store, &name, json),
        BaselineCommand::Refresh { name, yes } => {
            if !yes {
                bail!("baseline refresh requires --yes because it changes the safety reference");
            }
            let mut environment = store.load_environment(&name)?;
            environment.baseline = capture_baseline()?;
            environment.updated_at = Utc::now();
            store.save_environment(&environment)?;
            println!(
                "Captured {} connection capabilities for '{}'.",
                environment.baseline.len(),
                name
            );
            Ok(0)
        }
    }
}

fn connections(store: &Store, name: &str, json: bool) -> Result<i32> {
    let environment = store.load_environment(name)?;
    if json {
        print_json(&environment.baseline)?;
    } else if environment.baseline.is_empty() {
        println!("No connection capabilities were captured.");
    } else {
        println!(
            "{:<18} {:<9} {:<12} {:<22} REMOTE",
            "ID", "DIRECTION", "RESULT", "LOCAL"
        );
        for capability in environment.baseline {
            println!(
                "{:<18} {:<9} {:<12} {:<22} {}",
                capability.id,
                capability.direction,
                capability.validation,
                capability.local,
                capability.remote
            );
            if let Some(name) = capability.name {
                println!("  name: {name}");
            }
            if let Some(detail) = capability.detail {
                println!("  {detail}");
            }
        }
    }
    Ok(0)
}

fn circuit(store: &Store, command: CircuitCommand) -> Result<i32> {
    match command {
        CircuitCommand::Add {
            environment,
            name,
            command,
        } => {
            let argv = utf8_arguments(command)?;
            let joined = argv.join("\0");
            let id =
                crate::connectivity::capability_id("command", &Direction::Outbound, &name, &joined);
            let capability = Capability {
                id: id.clone(),
                name: Some(name),
                protocol: "application".into(),
                direction: Direction::Outbound,
                local: "sandbox".into(),
                remote: argv.join(" "),
                process: None,
                probe: ProbeSpec::Command { argv },
                required: true,
                validation: ValidationState::Pending,
                detail: Some("user or agent supplied application-level canary".into()),
                last_checked_at: None,
            };
            add_capability(store, &environment, capability)?;
            println!("Added required circuit '{id}' to '{environment}'.");
        }
        CircuitCommand::AddTcp {
            environment,
            name,
            endpoint,
            interface,
        } => {
            endpoint
                .parse::<std::net::SocketAddr>()
                .context("endpoint must be an IP address and port, such as 1.1.1.1:443")?;
            let id = crate::connectivity::capability_id(
                "tcp",
                &Direction::Outbound,
                &name,
                &format!("{}@{}", endpoint, interface.as_deref().unwrap_or("default")),
            );
            let capability = Capability {
                id: id.clone(),
                name: Some(name),
                protocol: "tcp".into(),
                direction: Direction::Outbound,
                local: interface
                    .as_ref()
                    .map_or_else(|| "sandbox".into(), |name| format!("sandbox@{name}")),
                remote: endpoint.clone(),
                process: None,
                probe: ProbeSpec::Tcp {
                    endpoint,
                    interface,
                },
                required: true,
                validation: ValidationState::Pending,
                detail: Some("manually supplied TCP canary".into()),
                last_checked_at: None,
            };
            add_capability(store, &environment, capability)?;
            println!("Added required circuit '{id}' to '{environment}'.");
        }
        CircuitCommand::Ignore { environment, id } => {
            update_capability(store, &environment, &id, |capability| {
                capability.required = false;
                capability.validation = ValidationState::Ignored;
                capability.detail = Some("explicitly ignored".into());
            })?;
            println!("Circuit '{id}' is now non-blocking.");
        }
        CircuitCommand::Require { environment, id } => {
            update_capability(store, &environment, &id, |capability| {
                capability.required = true;
                capability.validation = ValidationState::Pending;
                capability.detail = Some("required; awaiting validation".into());
            })?;
            println!("Circuit '{id}' is now required.");
        }
        CircuitCommand::Remove { environment, id } => {
            let mut sandbox = store.load_environment(&environment)?;
            let before = sandbox.baseline.len();
            sandbox.baseline.retain(|capability| capability.id != id);
            if sandbox.baseline.len() == before {
                bail!("circuit '{id}' does not exist");
            }
            sandbox.updated_at = Utc::now();
            store.save_environment(&sandbox)?;
            println!("Removed circuit '{id}'.");
        }
    }
    Ok(0)
}

fn mac(store: &Store, command: MacCommand, json: bool) -> Result<i32> {
    if !cfg!(target_os = "macos") {
        bail!("the macOS candidate backend is available only on macOS");
    }
    match command {
        MacCommand::LinuxCreate {
            environment,
            image,
            guest_binary,
        } => {
            let mut sandbox = store.load_environment(&environment)?;
            ensure_enterable(&sandbox)?;
            if platform::has_guest_runtime(&sandbox) {
                bail!("environment '{environment}' already has a guest runtime");
            }
            let guest_binary = match guest_binary {
                Some(path) => path,
                None => platform::mac_linux_guest_binary(store)?,
            };
            platform::mac_linux_create(&mut sandbox, store, &image, &guest_binary)?;
            sandbox.updated_at = Utc::now();
            if let Err(error) = store.save_environment(&sandbox) {
                let cleanup = platform::cleanup_environment(&sandbox);
                return match cleanup {
                    Ok(_) => Err(error).context("save Linux image environment"),
                    Err(cleanup_error) => Err(error).context(format!(
                        "save Linux image environment; container cleanup also failed: {cleanup_error:#}"
                    )),
                };
            }
            if json {
                print_json(&platform::mac_linux_status(&sandbox)?)?;
            } else {
                println!("Created lightweight Linux image environment '{environment}'.");
                println!("Source image: {image}");
                println!("The source image and Mac host were not changed.");
                println!(
                    "Track files before mutation with: netsandbox mac linux-track {environment} /PATH"
                );
            }
        }
        MacCommand::LinuxStatus { environment } => {
            let sandbox = store.load_environment(&environment)?;
            let status = platform::mac_linux_status(&sandbox)?;
            if json {
                print_json(&status)?;
            } else {
                println!("{}", serde_json::to_string_pretty(&status)?);
            }
        }
        MacCommand::LinuxTrack { environment, path } => {
            let mut sandbox = store.load_environment(&environment)?;
            ensure_enterable(&sandbox)?;
            if !platform::is_linux_image_runtime(&sandbox) {
                bail!("environment '{environment}' has no Linux image runtime");
            }
            let relative = normalize_user_path(&path)?;
            platform::mac_linux_track(&mut sandbox, store, &relative)?;
            sandbox.updated_at = Utc::now();
            store.save_environment(&sandbox)?;
            println!(
                "Captured and tracked Linux image file /{}.",
                relative.display()
            );
        }
        MacCommand::LinuxSync { environment } => {
            let mut sandbox = store.load_environment(&environment)?;
            if !platform::is_linux_image_runtime(&sandbox) {
                bail!("environment '{environment}' has no Linux image runtime");
            }
            let count = platform::sync_isolated_runtime(&mut sandbox, store)?;
            sandbox.updated_at = Utc::now();
            store.save_environment(&sandbox)?;
            if json {
                print_json(&serde_json::json!({
                    "environment": environment,
                    "synced_paths": count,
                }))?;
            } else {
                println!("Exported {count} tracked Linux image file(s).");
            }
        }
        MacCommand::LinuxDiff { environment } => {
            let sandbox = store.load_environment(&environment)?;
            let changes = platform::mac_linux_changes(&sandbox)?;
            if json {
                print_json(&changes)?;
            } else if changes.is_empty() {
                println!("No Linux image changes.");
            } else {
                for change in changes {
                    println!(
                        "{:<2} {:<9} {}",
                        change.kind,
                        if change.tracked {
                            "tracked"
                        } else {
                            "UNTRACKED"
                        },
                        change.path.display()
                    );
                }
            }
        }
        MacCommand::LinuxReset { environment, yes } => {
            if !yes {
                bail!("resetting a Linux image environment requires --yes");
            }
            let mut sandbox = store.load_environment(&environment)?;
            ensure_enterable(&sandbox)?;
            platform::mac_linux_reset(&sandbox)?;
            for path in sandbox.tracked_paths.clone() {
                remove_upper_path(&store.upper_dir(&environment).join(&path))?;
                sandbox.deleted_paths.retain(|deleted| deleted != &path);
            }
            sandbox.status = EnvironmentStatus::Ready;
            sandbox.updated_at = Utc::now();
            store.save_environment(&sandbox)?;
            println!("Recreated '{environment}' from its original Linux image.");
            println!("The source image and Mac host were not changed.");
        }
        MacCommand::LinuxCommit {
            environment,
            output_image,
            yes,
        } => {
            if !yes {
                bail!("committing a Linux image requires --yes");
            }
            let mut sandbox = store.load_environment(&environment)?;
            ensure_enterable(&sandbox)?;
            platform::sync_isolated_runtime(&mut sandbox, store)?;
            let blockers = validation_blockers(&sandbox);
            if !blockers.is_empty() {
                bail!(
                    "Linux image commit is blocked by connectivity validation: {}; run 'netsandbox check {}'",
                    blockers.join(", "),
                    sandbox.name
                );
            }
            let changes = platform::mac_linux_changes(&sandbox)?;
            if changes.is_empty() {
                bail!("Linux image has no changes to commit");
            }
            let id = platform::mac_linux_commit(&mut sandbox, &output_image)?;
            sandbox.status = EnvironmentStatus::Applied;
            sandbox.updated_at = Utc::now();
            if let Err(error) = store.save_environment(&sandbox) {
                let cleanup = platform::mac_linux_rollback(&mut sandbox);
                return match cleanup {
                    Ok(_) => Err(error).context("save committed Linux image metadata"),
                    Err(cleanup_error) => Err(error).context(format!(
                        "save committed Linux image metadata; output image cleanup also failed: {cleanup_error:#}"
                    )),
                };
            }
            if json {
                print_json(&serde_json::json!({
                    "environment": environment,
                    "image": output_image,
                    "id": id,
                }))?;
            } else {
                println!("Committed validated Linux image '{output_image}'.");
                println!("Image ID: {id}");
                println!("The original image and Mac host were not changed.");
            }
        }
        MacCommand::LinuxRollback { environment, yes } => {
            if !yes {
                bail!("removing a committed Linux image requires --yes");
            }
            let mut sandbox = store.load_environment(&environment)?;
            let image = platform::mac_linux_rollback(&mut sandbox)?;
            sandbox.status = EnvironmentStatus::Ready;
            sandbox.updated_at = Utc::now();
            store.save_environment(&sandbox)?;
            println!("Removed committed Linux image '{image}'.");
            println!("The original source image remains unchanged.");
        }
        MacCommand::VmClone {
            environment,
            base_instance,
        } => {
            let mut sandbox = store.load_environment(&environment)?;
            ensure_enterable(&sandbox)?;
            platform::mac_vm_clone(&mut sandbox, &base_instance)?;
            sandbox.updated_at = Utc::now();
            store.save_environment(&sandbox)?;
            let status = platform::mac_vm_status(&sandbox)?;
            if json {
                print_json(&status)?;
            } else {
                println!("Created isolated macOS VM for '{environment}'.");
                println!("Enter with: netsandbox enter {environment}");
            }
        }
        MacCommand::VmAttach {
            environment,
            instance,
        } => {
            let mut sandbox = store.load_environment(&environment)?;
            ensure_enterable(&sandbox)?;
            platform::mac_vm_attach(&mut sandbox, &instance)?;
            sandbox.updated_at = Utc::now();
            store.save_environment(&sandbox)?;
            if json {
                print_json(&platform::mac_vm_status(&sandbox)?)?;
            } else {
                println!("Attached macOS VM '{instance}' to '{environment}'.");
                println!("Enter with: netsandbox enter {environment}");
            }
        }
        MacCommand::VmStatus { environment } => {
            let sandbox = store.load_environment(&environment)?;
            let status = platform::mac_vm_status(&sandbox)?;
            if json {
                print_json(&status)?;
            } else {
                println!("{}", serde_json::to_string_pretty(&status)?);
            }
        }
        MacCommand::VmTrack { environment, path } => {
            let mut sandbox = store.load_environment(&environment)?;
            ensure_enterable(&sandbox)?;
            if !platform::is_macos_vm_runtime(&sandbox) {
                bail!("environment '{environment}' has no isolated macOS VM");
            }
            let relative = normalize_user_path(&path)?;
            sandbox
                .origins
                .entry(relative.clone())
                .or_insert(inspect_origin(&sandbox.base_root.join(&relative))?);
            if !sandbox.tracked_paths.contains(&relative) {
                sandbox.tracked_paths.push(relative.clone());
                sandbox.tracked_paths.sort();
            }
            sandbox.updated_at = Utc::now();
            store.save_environment(&sandbox)?;
            println!(
                "Tracking guest path /{} for differential export.",
                relative.display()
            );
            println!("The prepared base VM must represent the host baseline for this path.");
        }
        MacCommand::VmSync { environment } => {
            let mut sandbox = store.load_environment(&environment)?;
            let count = platform::mac_vm_sync(&mut sandbox, store)?;
            sandbox.updated_at = Utc::now();
            store.save_environment(&sandbox)?;
            if json {
                print_json(&serde_json::json!({
                    "environment": environment,
                    "synced_paths": count,
                }))?;
            } else {
                println!("Exported {count} tracked path(s) from the isolated macOS VM.");
            }
        }
        MacCommand::VmReset { environment, yes } => {
            if !yes {
                bail!("resetting a macOS VM clone requires --yes");
            }
            let mut sandbox = store.load_environment(&environment)?;
            ensure_enterable(&sandbox)?;
            platform::mac_vm_reset(&sandbox)?;
            for path in sandbox.tracked_paths.clone() {
                remove_upper_path(&store.upper_dir(&environment).join(&path))?;
                sandbox.deleted_paths.retain(|deleted| deleted != &path);
                sandbox.origins.remove(&path);
            }
            sandbox.status = EnvironmentStatus::Ready;
            sandbox.updated_at = Utc::now();
            store.save_environment(&sandbox)?;
            println!("Recreated the isolated macOS VM for '{environment}' from its base.");
            println!("Cleared its tracked differential exports; the host was not changed.");
        }
        MacCommand::RouteShow { destination } => {
            let observation = platform::observe_route(&destination)?;
            if json {
                print_json(&observation)?;
            } else {
                println!("Destination: {}", observation.destination);
                println!(
                    "Interface:   {}",
                    observation.interface.as_deref().unwrap_or("unknown")
                );
                println!(
                    "Gateway:     {}",
                    observation.gateway.as_deref().unwrap_or("none")
                );
                println!("Flags:       {}", observation.flags.join(","));
            }
        }
        MacCommand::RoutePreview {
            environment,
            destination,
            interface,
            gateway,
            mut ports,
        } => {
            let ip = destination
                .parse::<std::net::IpAddr>()
                .context("route-preview destination must be an IP address")?;
            platform::validate_interface(&interface)?;
            if let Some(gateway) = &gateway {
                let gateway_ip = gateway
                    .parse::<std::net::IpAddr>()
                    .context("route-preview gateway must be an IP address")?;
                if gateway_ip.is_ipv4() != ip.is_ipv4() {
                    bail!("route-preview destination and gateway must use the same address family");
                }
            }
            ports.sort_unstable();
            ports.dedup();
            if ports.contains(&0) {
                bail!("port zero is not a valid circuit endpoint");
            }
            let observed_route = platform::observe_route(&destination)?;
            let candidate_key = format!(
                "{}@{}@{}:{}",
                destination,
                interface,
                gateway.as_deref().unwrap_or("direct"),
                ports
                    .iter()
                    .map(u16::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            );
            let candidate_id = crate::connectivity::capability_id(
                "mac_route",
                &Direction::Outbound,
                &environment,
                &candidate_key,
            );
            let mut sandbox = store.load_environment(&environment)?;
            if sandbox
                .route_candidates
                .iter()
                .any(|candidate| candidate.id == candidate_id)
            {
                bail!("route candidate '{candidate_id}' already exists");
            }

            let mut capability_ids = Vec::new();
            for port in &ports {
                let endpoint = std::net::SocketAddr::new(ip, *port).to_string();
                let id = crate::connectivity::capability_id(
                    "mac_route_tcp",
                    &Direction::Outbound,
                    &candidate_id,
                    &endpoint,
                );
                capability_ids.push(id.clone());
                sandbox.baseline.push(Capability {
                    id,
                    name: Some(format!("route-preview-{destination}-{port}")),
                    protocol: "tcp".into(),
                    direction: Direction::Outbound,
                    local: format!("candidate@{interface}"),
                    remote: endpoint.clone(),
                    process: None,
                    probe: ProbeSpec::Tcp {
                        endpoint,
                        interface: Some(interface.clone()),
                    },
                    required: true,
                    validation: ValidationState::Pending,
                    detail: Some(
                        "macOS interface-bound candidate; the live route was not changed".into(),
                    ),
                    last_checked_at: None,
                });
            }
            let candidate = RouteCandidate {
                id: candidate_id.clone(),
                destination,
                interface,
                gateway,
                ports,
                created_at: Utc::now(),
                observed_route,
                capability_ids,
            };
            sandbox.route_candidates.push(candidate.clone());
            sandbox.updated_at = Utc::now();
            store.save_environment(&sandbox)?;
            if json {
                print_json(&candidate)?;
            } else {
                println!("Staged macOS route candidate '{candidate_id}'.");
                println!(
                    "Current route: {} via {}",
                    candidate
                        .observed_route
                        .interface
                        .as_deref()
                        .unwrap_or("unknown"),
                    candidate
                        .observed_route
                        .gateway
                        .as_deref()
                        .unwrap_or("no gateway")
                );
                println!(
                    "Candidate:     {} via {}{}",
                    candidate.destination,
                    candidate.interface,
                    candidate
                        .gateway
                        .as_ref()
                        .map_or_else(String::new, |gateway| format!(" gateway {gateway}"))
                );
                println!("The host routing table was not changed.");
                println!(
                    "After testing, preview apply with: netsandbox apply {} --dry-run",
                    sandbox.name
                );
            }
        }
        MacCommand::RouteList { environment } => {
            let sandbox = store.load_environment(&environment)?;
            if json {
                print_json(&sandbox.route_candidates)?;
            } else if sandbox.route_candidates.is_empty() {
                println!("No macOS route candidates.");
            } else {
                println!(
                    "{:<18} {:<18} {:<12} {:<18} PORTS",
                    "ID", "DESTINATION", "INTERFACE", "GATEWAY"
                );
                for candidate in sandbox.route_candidates {
                    println!(
                        "{:<18} {:<18} {:<12} {:<18} {}",
                        candidate.id,
                        candidate.destination,
                        candidate.interface,
                        candidate.gateway.as_deref().unwrap_or("direct"),
                        candidate
                            .ports
                            .iter()
                            .map(u16::to_string)
                            .collect::<Vec<_>>()
                            .join(",")
                    );
                }
            }
        }
        MacCommand::Test {
            environment,
            timeout,
        } => {
            let mut sandbox = store.load_environment(&environment)?;
            let candidate_ids = sandbox
                .route_candidates
                .iter()
                .flat_map(|candidate| candidate.capability_ids.iter().cloned())
                .collect::<std::collections::BTreeSet<_>>();
            if candidate_ids.is_empty() {
                bail!("environment '{environment}' has no macOS route candidates");
            }
            let guarded_trial_ids = sandbox
                .route_candidates
                .iter()
                .filter(|candidate| route_candidate_needs_guarded_trial(candidate))
                .flat_map(|candidate| candidate.capability_ids.iter().cloned())
                .collect::<std::collections::BTreeSet<_>>();
            let timeout = Duration::from_secs(timeout.max(1));
            for capability in &mut sandbox.baseline {
                if guarded_trial_ids.contains(&capability.id) {
                    capability.validation = ValidationState::Unverifiable;
                    capability.detail = Some(
                        "the staged host route differs from the live route; macOS cannot select that exact route per socket, so this check is deferred to a guarded route trial"
                            .into(),
                    );
                    capability.last_checked_at = Some(Utc::now());
                } else {
                    validate_capabilities(std::slice::from_mut(capability), timeout);
                }
            }
            let failed = sandbox.baseline.iter().any(|capability| {
                capability.required && capability.validation != ValidationState::Preserved
            });
            sandbox.status = if failed {
                EnvironmentStatus::ValidationFailed
            } else {
                EnvironmentStatus::Ready
            };
            sandbox.updated_at = Utc::now();
            store.save_environment(&sandbox)?;
            connections(store, &environment, json)?;
            if !json {
                println!("The host routing table was not changed.");
                if !guarded_trial_ids.is_empty() {
                    println!(
                        "Exact candidate-route checks require a guarded live trial. Preview it with: netsandbox apply {environment} --dry-run --trial"
                    );
                } else if !failed {
                    println!(
                        "Candidate checks passed. Apply with explicit approval: sudo netsandbox apply {environment} --yes"
                    );
                }
            }
            return Ok(if failed { 2 } else { 0 });
        }
        MacCommand::RouteCanary {
            environment,
            candidate,
            name,
            command,
        } => {
            let argv = utf8_arguments(command)?;
            let joined = argv.join("\0");
            let mut sandbox = store.load_environment(&environment)?;
            let position = sandbox
                .route_candidates
                .iter()
                .position(|route| route.id == candidate)
                .with_context(|| format!("route candidate '{candidate}' does not exist"))?;
            let id = crate::connectivity::capability_id(
                "mac_route_command",
                &Direction::Outbound,
                &candidate,
                &joined,
            );
            if sandbox
                .baseline
                .iter()
                .any(|capability| capability.id == id)
            {
                bail!("that route canary already exists");
            }
            let interface = sandbox.route_candidates[position].interface.clone();
            sandbox.route_candidates[position]
                .capability_ids
                .push(id.clone());
            sandbox.baseline.push(Capability {
                id: id.clone(),
                name: Some(name),
                protocol: "application".into(),
                direction: Direction::Outbound,
                local: format!("candidate@{interface}"),
                remote: argv.join(" "),
                process: None,
                probe: ProbeSpec::Command { argv },
                required: true,
                validation: ValidationState::Pending,
                detail: Some(format!(
                    "application canary associated with route candidate {candidate}; it is rechecked after live apply"
                )),
                last_checked_at: None,
            });
            sandbox.updated_at = Utc::now();
            store.save_environment(&sandbox)?;
            println!("Added required application canary '{id}' to candidate '{candidate}'.");
        }
        MacCommand::RouteRemove {
            environment,
            candidate,
        } => {
            let mut sandbox = store.load_environment(&environment)?;
            let position = sandbox
                .route_candidates
                .iter()
                .position(|route| route.id == candidate)
                .with_context(|| format!("route candidate '{candidate}' does not exist"))?;
            let removed = sandbox.route_candidates.remove(position);
            sandbox
                .baseline
                .retain(|capability| !removed.capability_ids.contains(&capability.id));
            sandbox.updated_at = Utc::now();
            store.save_environment(&sandbox)?;
            println!(
                "Removed candidate '{}'; the live route was not touched.",
                removed.id
            );
        }
    }
    Ok(0)
}

fn add_capability(store: &Store, environment: &str, capability: Capability) -> Result<()> {
    let mut sandbox = store.load_environment(environment)?;
    if sandbox
        .baseline
        .iter()
        .any(|existing| existing.id == capability.id)
    {
        bail!("that circuit already exists");
    }
    sandbox.baseline.push(capability);
    sandbox.updated_at = Utc::now();
    store.save_environment(&sandbox)
}

fn utf8_arguments(command: Vec<OsString>) -> Result<Vec<String>> {
    command
        .into_iter()
        .map(|argument| {
            argument
                .into_string()
                .map_err(|_| anyhow::anyhow!("probe arguments must be valid UTF-8"))
        })
        .collect()
}

fn update_capability(
    store: &Store,
    environment: &str,
    id: &str,
    update: impl FnOnce(&mut Capability),
) -> Result<()> {
    let mut sandbox = store.load_environment(environment)?;
    let capability = sandbox
        .baseline
        .iter_mut()
        .find(|capability| capability.id == id)
        .with_context(|| format!("circuit '{id}' does not exist"))?;
    update(capability);
    sandbox.updated_at = Utc::now();
    store.save_environment(&sandbox)
}

fn check(store: Store, args: CheckArgs, json: bool) -> Result<i32> {
    let active = std::env::var("NETSANDBOX_ACTIVE").ok();
    let environment = store.load_environment(&args.name)?;
    let mac_native_probe =
        cfg!(target_os = "macos") && !platform::has_isolated_runtime(&environment);
    if mac_native_probe || args.current_namespace || active.as_deref() == Some(args.name.as_str()) {
        return probe(&store, &args.name, args.timeout, json);
    }
    prepare_control(&store, &environment)?;
    let state_root = store.root().to_path_buf();
    let executable = std::env::current_exe()?;
    let command = vec![
        executable.into_os_string(),
        OsString::from("__probe"),
        OsString::from(&args.name),
        OsString::from("--timeout"),
        OsString::from(args.timeout.to_string()),
    ];
    drop(store);
    let code = platform::run_in_environment(&environment, &state_root, &command)?;
    let store = Store::open(Some(state_root))?;
    let mut current = store.load_environment(&environment.name)?;
    import_probe_result(&store, &mut current)?;
    current.updated_at = Utc::now();
    store.save_environment(&current)?;
    Ok(code)
}

fn watch(mut store: Store, args: WatchArgs, json: bool) -> Result<i32> {
    let state_root = store.root().to_path_buf();
    let mut iteration = 0_u32;
    loop {
        iteration += 1;
        let active = std::env::var("NETSANDBOX_ACTIVE").ok();
        let environment = store.load_environment(&args.name)?;
        let direct = active.as_deref() == Some(args.name.as_str())
            || cfg!(target_os = "macos") && !platform::has_isolated_runtime(&environment);
        let code = if direct {
            probe(&store, &args.name, args.timeout, json)?
        } else {
            drop(store);
            let owned = Store::open(Some(state_root.clone()))?;
            let code = check(
                owned,
                CheckArgs {
                    name: args.name.clone(),
                    timeout: args.timeout,
                    current_namespace: false,
                },
                json,
            )?;
            store = Store::open(Some(state_root.clone()))?;
            code
        };
        if args.count != 0 && iteration >= args.count {
            return Ok(code);
        }
        drop(store);
        std::thread::sleep(Duration::from_secs(args.interval.max(1)));
        store = Store::open(Some(state_root.clone()))?;
    }
}

fn probe(store: &Store, name: &str, timeout: u64, json: bool) -> Result<i32> {
    let mut environment = store.load_environment(name)?;
    validate_capabilities(
        &mut environment.baseline,
        Duration::from_secs(timeout.max(1)),
    );
    let failed = environment.baseline.iter().any(|capability| {
        capability.required
            && matches!(
                capability.validation,
                ValidationState::Lost | ValidationState::Unverifiable
            )
    });
    environment.status = if failed {
        EnvironmentStatus::ValidationFailed
    } else {
        EnvironmentStatus::Ready
    };
    environment.updated_at = Utc::now();
    store.save_environment(&environment)?;
    connections(store, name, json)?;
    Ok(if failed { 2 } else { 0 })
}

fn validation_blockers(environment: &Environment) -> Vec<String> {
    environment
        .baseline
        .iter()
        .filter(|capability| {
            let stale = capability
                .last_checked_at
                .is_none_or(|checked| Utc::now().signed_duration_since(checked).num_seconds() > 60);
            capability.required && (stale || capability.validation != ValidationState::Preserved)
        })
        .map(|capability| capability.id.clone())
        .collect()
}

fn plan(store: &Store, name: &str, json: bool) -> Result<i32> {
    let mut environment = store.load_environment(name)?;
    if !platform::supports_host_apply(&environment) {
        bail!(
            "a Linux image environment cannot be applied to the Mac host; use 'netsandbox mac linux-diff {name}' and 'linux-commit'"
        );
    }
    if cfg!(target_os = "macos") && platform::has_isolated_runtime(&environment) {
        platform::sync_isolated_runtime(&mut environment, store)?;
    }
    let plan = build_plan(store, &mut environment)?;
    store.save_environment(&environment)?;
    if json {
        print_json(&plan)?;
    } else {
        print_plan(&plan);
    }
    Ok(if plan.allowed { 0 } else { 2 })
}

fn apply(store: &Store, args: ApplyArgs, json: bool) -> Result<i32> {
    let mut environment = store.load_environment(&args.name)?;
    if !platform::supports_host_apply(&environment) {
        bail!(
            "a Linux image environment cannot be applied to the Mac host; use 'netsandbox mac linux-diff {}' and 'linux-commit'",
            args.name
        );
    }
    if cfg!(target_os = "macos") && platform::has_isolated_runtime(&environment) {
        platform::sync_isolated_runtime(&mut environment, store)?;
    }
    if args.dry_run {
        let plan = if args.trial {
            build_guarded_trial_plan(store, &mut environment)?
        } else {
            build_plan(store, &mut environment)?
        };
        store.save_environment(&environment)?;
        if json {
            print_json(&plan)?;
        } else {
            print_plan(&plan);
        }
        return Ok(if plan.allowed { 0 } else { 2 });
    }
    platform::require_real_host_apply(&environment)?;
    if !args.yes {
        bail!("applying host changes requires --yes");
    }
    let refreshed =
        refresh_pre_apply_capabilities(&mut environment, args.trial, Duration::from_secs(5));
    let plan = if args.trial {
        build_guarded_trial_plan(store, &mut environment)?
    } else {
        build_plan(store, &mut environment)?
    };
    store.save_environment(&environment)?;
    if !json {
        println!(
            "Refreshed {refreshed} required {} immediately before apply.",
            if refreshed == 1 {
                "circuit"
            } else {
                "circuits"
            }
        );
    }
    if !plan.allowed {
        print_plan(&plan);
        bail!("apply is blocked");
    }
    if plan.guarded_trial && !json {
        println!(
            "Starting guarded route trial; rollback protection will be armed before route mutation."
        );
    }
    let (mut transaction, lease) = apply_plan(store, &environment, &plan)?;
    validate_capabilities(&mut environment.baseline, Duration::from_secs(5));
    let failed_routes = match verify_route_changes(&transaction) {
        Ok(failures) => failures,
        Err(error) if environment.policy.auto_rollback => {
            rollback_transaction(store, &mut transaction)?;
            lease.finish()?;
            environment.status = EnvironmentStatus::ValidationFailed;
            environment.updated_at = Utc::now();
            store.save_environment(&environment)?;
            return Err(error)
                .context("post-apply route verification failed; changes were rolled back");
        }
        Err(error) => {
            return Err(error).context("post-apply route verification failed");
        }
    };
    let post_apply_failed = !failed_routes.is_empty()
        || environment.baseline.iter().any(|capability| {
            capability.required && capability.validation != ValidationState::Preserved
        });
    if post_apply_failed {
        environment.status = EnvironmentStatus::ValidationFailed;
        environment.updated_at = Utc::now();
        if environment.policy.auto_rollback {
            rollback_transaction(store, &mut transaction)?;
            lease.finish()?;
            store.save_environment(&environment)?;
            bail!(
                "post-apply circuit validation failed; host changes were automatically rolled back"
            );
        }
        store.save_environment(&environment)?;
        bail!("post-apply circuit validation failed; automatic rollback is disabled");
    }
    environment.status = EnvironmentStatus::Applied;
    environment.applied_transaction = Some(transaction.id.clone());
    environment.updated_at = Utc::now();
    store.save_environment(&environment)?;
    lease.commit(store, &transaction.id)?;
    if json {
        print_json(&transaction)?;
    } else {
        println!("Applied '{}'.", environment.name);
        println!("Rollback transaction: {}", transaction.id);
    }
    Ok(0)
}

fn discard(store: &Store, args: ConfirmNameArgs) -> Result<i32> {
    let environment = store.load_environment(&args.name)?;
    if environment.status == EnvironmentStatus::Applied {
        bail!("an applied environment must be rolled back or removed from history");
    }
    if !args.yes {
        bail!("discarding unapplied changes requires --yes");
    }
    let runtime = platform::runtime_description(&environment);
    let removed_runtime = platform::cleanup_environment(&environment)?;
    store.delete_environment(&args.name)?;
    println!("Discarded '{}'; the host was not changed.", args.name);
    if removed_runtime {
        println!(
            "Removed its managed {}.",
            runtime.unwrap_or("isolated runtime")
        );
    }
    Ok(0)
}

fn remove(store: &Store, args: ConfirmNameArgs) -> Result<i32> {
    let mut environment = store.load_environment(&args.name)?;
    let changes = scan_changes(&mut environment, &store.upper_dir(&args.name))?;
    if !changes.is_empty()
        && environment.status != EnvironmentStatus::Applied
        && environment.status != EnvironmentStatus::Discarded
    {
        bail!("environment has unapplied changes; use discard --yes");
    }
    if !args.yes {
        bail!("removing an environment requires --yes");
    }
    let runtime = platform::runtime_description(&environment);
    let removed_runtime = platform::cleanup_environment(&environment)?;
    store.delete_environment(&args.name)?;
    println!("Removed '{}'.", args.name);
    if removed_runtime {
        println!(
            "Removed its managed {}.",
            runtime.unwrap_or("isolated runtime")
        );
    }
    Ok(0)
}

fn history(store: &Store, json: bool) -> Result<i32> {
    let transactions = store.list_transactions()?;
    if json {
        print_json(&transactions)?;
    } else if transactions.is_empty() {
        println!("No apply transactions.");
    } else {
        println!("{:<40} {:<24} STATUS", "TRANSACTION", "ENVIRONMENT");
        for transaction in transactions {
            let status = if transaction.rolled_back_at.is_some() {
                "rolled_back"
            } else {
                "applied"
            };
            println!(
                "{:<40} {:<24} {}",
                transaction.id, transaction.environment, status
            );
        }
    }
    Ok(0)
}

fn rollback(store: &Store, args: RollbackArgs) -> Result<i32> {
    if !args.yes {
        bail!("rolling back host changes requires --yes");
    }
    let mut transaction = store.load_transaction(&args.transaction)?;
    if transaction.base_root == Path::new("/") && !platform::is_privileged() {
        bail!("rolling back the real host requires root privileges");
    }
    rollback_transaction(store, &mut transaction)?;
    if let Ok(mut environment) = store.load_environment(&transaction.environment) {
        environment.status = EnvironmentStatus::Ready;
        environment.updated_at = Utc::now();
        store.save_environment(&environment)?;
    }
    println!("Rolled back '{}'.", transaction.id);
    Ok(0)
}

fn rollback_guard(store: &Store, transaction_id: &str, timeout: u64) -> Result<i32> {
    let timeout = Duration::from_secs(timeout.max(5));
    let lease = store.transaction_lease_path(transaction_id)?;
    loop {
        if store.transaction_is_committed(transaction_id)? {
            return Ok(0);
        }
        let mut transaction = store.load_transaction(transaction_id)?;
        if transaction.rolled_back_at.is_some() {
            return Ok(0);
        }
        let age = fs::metadata(&lease)
            .and_then(|metadata| metadata.modified())
            .and_then(|modified| modified.elapsed().map_err(std::io::Error::other))
            .unwrap_or(timeout);
        if age >= timeout {
            rollback_transaction(store, &mut transaction)?;
            return Ok(0);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn doctor(json: bool) -> Result<i32> {
    let checks = platform::doctor();
    let failed = checks.iter().any(|check| check.required && !check.ok);
    if json {
        let values = checks
            .iter()
            .map(|check| {
                serde_json::json!({
                    "name": check.name,
                    "ok": check.ok,
                    "required": check.required,
                    "detail": check.detail,
                })
            })
            .collect::<Vec<_>>();
        print_json(&values)?;
    } else {
        for check in checks {
            let result = if check.ok {
                "PASS"
            } else if check.required {
                "FAIL"
            } else {
                "INFO"
            };
            println!("{:<5} {:<22} {}", result, check.name, check.detail);
        }
    }
    Ok(if failed { 2 } else { 0 })
}

fn ensure_enterable(environment: &Environment) -> Result<()> {
    if matches!(
        environment.status,
        EnvironmentStatus::Applied | EnvironmentStatus::Discarded
    ) {
        bail!(
            "environment '{}' is {} and cannot be entered",
            environment.name,
            environment.status
        );
    }
    Ok(())
}

fn normalize_user_path(path: &Path) -> Result<PathBuf> {
    let normalized = if path.is_absolute() {
        path.strip_prefix("/")?.to_path_buf()
    } else {
        path.to_path_buf()
    };
    validate_relative_path(&normalized)?;
    Ok(normalized)
}

fn route_candidate_needs_guarded_trial(candidate: &RouteCandidate) -> bool {
    candidate.observed_route.interface.as_deref() != Some(candidate.interface.as_str())
        || candidate.observed_route.gateway != candidate.gateway
}

fn refresh_pre_apply_capabilities(
    environment: &mut Environment,
    guarded_trial: bool,
    timeout: Duration,
) -> usize {
    let deferred_ids = if guarded_trial {
        environment
            .route_candidates
            .iter()
            .flat_map(|candidate| candidate.capability_ids.iter().cloned())
            .collect::<std::collections::BTreeSet<_>>()
    } else {
        std::collections::BTreeSet::new()
    };
    let mut refreshed = 0;
    for capability in &mut environment.baseline {
        if capability.required && !deferred_ids.contains(&capability.id) {
            validate_capabilities(std::slice::from_mut(capability), timeout);
            refreshed += 1;
        }
    }
    refreshed
}

fn remove_upper_path(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => fs::remove_dir_all(path)?,
        Ok(_) => fs::remove_file(path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn print_plan(plan: &crate::model::ApplyPlan) {
    println!("Environment:            {}", plan.environment);
    println!(
        "Validation mode:         {}",
        if plan.guarded_trial {
            "GUARDED ROUTE TRIAL"
        } else {
            "NORMAL"
        }
    );
    println!("Filesystem changes:     {}", plan.changes.len());
    println!("Route changes:          {}", plan.route_candidates.len());
    println!("Post-apply checks:       {}", plan.deferred_required.len());
    println!("Lost required circuits: {}", plan.lost_required.len());
    println!(
        "Unverifiable circuits:  {}",
        plan.unverifiable_required.len()
    );
    println!("Host conflicts:         {}", plan.conflicts.len());
    println!("Route conflicts:        {}", plan.route_conflicts.len());
    for conflict in &plan.route_conflicts {
        println!("  route blocker: {conflict}");
    }
    println!(
        "Result:                 {}",
        if plan.allowed {
            if plan.guarded_trial {
                "READY FOR GUARDED TRIAL"
            } else {
                "READY TO APPLY"
            }
        } else {
            "APPLY BLOCKED"
        }
    );
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn prepare_control(store: &Store, environment: &Environment) -> Result<()> {
    let control = store.environment_dir(&environment.name).join("control");
    fs::create_dir_all(&control)?;
    let input = serde_json::to_vec_pretty(environment)?;
    fs::write(control.join("probe-input.json"), input)?;
    match fs::remove_file(control.join("probe-result.json")) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn control_probe(control: &Path, timeout: u64, json: bool) -> Result<i32> {
    let bytes = fs::read(control.join("probe-input.json"))
        .context("read supervisor connectivity baseline")?;
    let mut environment: Environment = serde_json::from_slice(&bytes)?;
    validate_capabilities(
        &mut environment.baseline,
        Duration::from_secs(timeout.max(1)),
    );
    let failed = environment.baseline.iter().any(|capability| {
        capability.required
            && matches!(
                capability.validation,
                ValidationState::Lost | ValidationState::Unverifiable
            )
    });
    environment.status = if failed {
        EnvironmentStatus::ValidationFailed
    } else {
        EnvironmentStatus::Ready
    };
    environment.updated_at = Utc::now();
    fs::write(
        control.join("probe-result.json"),
        serde_json::to_vec_pretty(&environment)?,
    )?;
    if json {
        print_json(&environment.baseline)?;
    } else {
        print_capabilities(&environment);
    }
    Ok(if failed { 2 } else { 0 })
}

fn import_probe_result(store: &Store, environment: &mut Environment) -> Result<()> {
    let result = store
        .environment_dir(&environment.name)
        .join("control/probe-result.json");
    let bytes = match fs::read(&result) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let probed: Environment = serde_json::from_slice(&bytes)?;
    environment.baseline = probed.baseline;
    environment.status = probed.status;
    environment.updated_at = probed.updated_at;
    fs::remove_file(result)?;
    Ok(())
}

fn print_capabilities(environment: &Environment) {
    if environment.baseline.is_empty() {
        println!("No connection capabilities were captured.");
        return;
    }
    println!(
        "{:<18} {:<9} {:<12} {:<22} REMOTE",
        "ID", "DIRECTION", "RESULT", "LOCAL"
    );
    for capability in &environment.baseline {
        println!(
            "{:<18} {:<9} {:<12} {:<22} {}",
            capability.id,
            capability.direction,
            capability.validation,
            capability.local,
            capability.remote
        );
        if let Some(detail) = &capability.detail {
            println!("  {detail}");
        }
    }
}

#[cfg(target_os = "linux")]
fn platform_sandbox_init(
    environment: &Environment,
    store: &Store,
    command: &[OsString],
) -> Result<i32> {
    crate::platform::linux::sandbox_init(environment, store, command)
}

#[cfg(not(target_os = "linux"))]
fn platform_sandbox_init(
    _environment: &Environment,
    _store: &Store,
    _command: &[OsString],
) -> Result<i32> {
    bail!("sandbox initialization is supported only on Linux")
}
