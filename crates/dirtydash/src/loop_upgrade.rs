use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::cli::LoopUpgradeArgs;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Workflow {
    SingleThreadSubagent,
    OrchestratorCallback,
}

impl Workflow {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "single-thread-subagent" => Some(Self::SingleThreadSubagent),
            "orchestrator-callback" => Some(Self::OrchestratorCallback),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::SingleThreadSubagent => "single-thread-subagent",
            Self::OrchestratorCallback => "orchestrator-callback",
        }
    }
}

#[derive(Debug)]
struct LoopContext {
    stream_name: String,
    stream_slug: String,
    epic_id: String,
    workflow: Workflow,
    custom_run_prompt: String,
}

#[derive(Debug)]
struct PlannedFile {
    target: PathBuf,
    content: String,
}

#[derive(Debug, PartialEq, Eq)]
enum ActionStatus {
    Created,
    Updated,
    Unchanged,
}

#[derive(Debug)]
struct UpgradeAction {
    target: PathBuf,
    status: ActionStatus,
}

#[derive(Debug)]
struct UpgradeReport {
    dirtyloops_root: PathBuf,
    loop_dir: PathBuf,
    workflow: Workflow,
    actions: Vec<UpgradeAction>,
}

impl UpgradeReport {
    fn changed_count(&self) -> usize {
        self.actions
            .iter()
            .filter(|action| action.status != ActionStatus::Unchanged)
            .count()
    }

    fn unchanged_count(&self) -> usize {
        self.actions
            .iter()
            .filter(|action| action.status == ActionStatus::Unchanged)
            .count()
    }
}

pub fn run(args: LoopUpgradeArgs) -> Result<()> {
    let report = upgrade_loop(&args)?;
    print_report(&report, args.check || args.dry_run);

    if args.check && report.changed_count() > 0 {
        bail!(
            "loop upgrade required: {} file(s) differ",
            report.changed_count()
        );
    }

    Ok(())
}

fn upgrade_loop(args: &LoopUpgradeArgs) -> Result<UpgradeReport> {
    let loop_dir = absolute_path(&args.loop_dir)?;
    if !loop_dir.is_dir() {
        bail!("loop directory does not exist: {}", loop_dir.display());
    }

    let dirtyloops_root = resolve_dirtyloops_root(args.dirtyloops_root.as_deref())?;
    validate_dirtyloops_root(&dirtyloops_root)?;

    let context = LoopContext::load(&loop_dir)?;
    let mut planned = Vec::new();
    planned.push(render_run_loop(&dirtyloops_root, &loop_dir, &context)?);
    planned.extend(schema_files(&dirtyloops_root, &loop_dir)?);

    if context.workflow == Workflow::OrchestratorCallback {
        planned.push(render_implementation_prompt(
            &dirtyloops_root,
            &loop_dir,
            &context,
        )?);
        planned.push(render_review_prompt(&dirtyloops_root, &loop_dir)?);
    }

    let write_files = !args.check && !args.dry_run;
    let actions = apply_plan(planned, write_files)?;

    Ok(UpgradeReport {
        dirtyloops_root,
        loop_dir,
        workflow: context.workflow,
        actions,
    })
}

impl LoopContext {
    fn load(loop_dir: &Path) -> Result<Self> {
        let implementation_path = loop_dir.join("IMPLEMENT.md");
        let loop_state_path = loop_dir.join("loop-state.md");
        let run_prompt_path = loop_dir.join("prompts/run-loop.md");

        if !implementation_path.is_file() {
            bail!(
                "loop directory is missing IMPLEMENT.md: {}",
                implementation_path.display()
            );
        }
        if !loop_state_path.is_file() {
            bail!(
                "loop directory is missing loop-state.md: {}",
                loop_state_path.display()
            );
        }

        let implementation = read_to_string(&implementation_path)?;
        let loop_state = read_to_string(&loop_state_path)?;
        let run_prompt = fs::read_to_string(&run_prompt_path).unwrap_or_default();
        let combined = format!("{run_prompt}\n{implementation}\n{loop_state}");

        let workflow = find_workflow(&combined)
            .and_then(|raw| Workflow::parse(&raw))
            .context("could not identify workflow; expected `single-thread-subagent` or `orchestrator-callback`")?;
        let epic_id =
            find_epic_id(&combined).context("could not identify Beads epic id from loop docs")?;
        let stream_slug = loop_dir
            .file_name()
            .and_then(OsStr::to_str)
            .context("loop directory must have a UTF-8 stream name")?
            .to_string();
        let stream_name = find_run_loop_title(&run_prompt)
            .or_else(|| find_implementation_title(&implementation))
            .unwrap_or_else(|| titleize_slug(&stream_slug));
        let custom_run_prompt = section_after_heading(&run_prompt, "## Start Prompt")
            .filter(|prompt| !prompt.trim().is_empty())
            .unwrap_or_else(|| default_run_prompt(&stream_name, workflow, &epic_id, &stream_slug));

        Ok(Self {
            stream_name,
            stream_slug,
            epic_id,
            workflow,
            custom_run_prompt,
        })
    }
}

fn render_run_loop(
    dirtyloops_root: &Path,
    loop_dir: &Path,
    context: &LoopContext,
) -> Result<PlannedFile> {
    let common_template = read_to_string(
        &dirtyloops_root
            .join("templates")
            .join("common")
            .join("run-loop.md.template"),
    )?;
    let addendum_path = dirtyloops_root
        .join("templates")
        .join("workflows")
        .join(context.workflow.as_str())
        .join("run-loop-addendum.md.template");
    let workflow_addendum = read_to_string(&addendum_path)?;
    let content = render_template(
        &common_template,
        &[
            ("STREAM_NAME", context.stream_name.as_str()),
            ("WORKFLOW", context.workflow.as_str()),
            ("EPIC_ID", context.epic_id.as_str()),
            ("STREAM_SLUG", context.stream_slug.as_str()),
            ("WORKFLOW_ADDENDUM", workflow_addendum.trim()),
            ("CUSTOM_RUN_PROMPT", context.custom_run_prompt.trim()),
            ("MM_DD_YYYY", "mm-dd-yyyy"),
        ],
    );

    Ok(PlannedFile {
        target: loop_dir.join("prompts/run-loop.md"),
        content: ensure_trailing_newline(content),
    })
}

fn schema_files(dirtyloops_root: &Path, loop_dir: &Path) -> Result<Vec<PlannedFile>> {
    let source_dir = dirtyloops_root.join("schemas");
    let mut entries = fs::read_dir(&source_dir)
        .with_context(|| format!("reading dirtyloops schemas {}", source_dir.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("reading dirtyloops schemas {}", source_dir.display()))?;
    entries.sort_by_key(|entry| entry.file_name());

    let mut files = Vec::new();
    for entry in entries {
        let source = entry.path();
        if source.extension() != Some(OsStr::new("json")) || !source.is_file() {
            continue;
        }
        let file_name = source
            .file_name()
            .context("schema file must have a file name")?;
        files.push(PlannedFile {
            target: loop_dir.join("schemas").join(file_name),
            content: read_to_string(&source)?,
        });
    }

    Ok(files)
}

fn render_implementation_prompt(
    dirtyloops_root: &Path,
    loop_dir: &Path,
    context: &LoopContext,
) -> Result<PlannedFile> {
    let target = loop_dir.join("prompts/implementation-thread.md");
    let existing = fs::read_to_string(&target).unwrap_or_default();
    let template = read_to_string(&dirtyloops_root.join(
        "templates/workflows/orchestrator-callback/implementation-thread-prompt.md.template",
    ))?;
    let phase_issue_id = find_prompt_issue(&existing, "implementation")
        .unwrap_or_else(|| "{{PHASE_ISSUE_ID}}".to_string());
    let orchestrator_thread_id =
        find_callback_target(&existing).unwrap_or_else(|| "THREAD_ORCHESTRATOR_ID".to_string());
    let phase_doc =
        find_bullet_value(&existing, &["Phase doc"]).unwrap_or_else(|| "{{PHASE_DOC}}".to_string());
    let implement_md = find_bullet_value(&existing, &["Implementation index"])
        .unwrap_or_else(|| format!("docs/implementation/{}/IMPLEMENT.md", context.stream_slug));
    let turn_doc =
        find_bullet_value(&existing, &["Turn doc"]).unwrap_or_else(|| "{{TURN_DOC}}".to_string());
    let branch_worktree_instructions = find_bullet_value(
        &existing,
        &["Branch/worktree instructions", "Branch policy"],
    )
    .unwrap_or_else(|| "{{BRANCH_WORKTREE_INSTRUCTIONS}}".to_string());

    let content = render_template(
        &template,
        &[
            ("PHASE_ISSUE_ID", phase_issue_id.as_str()),
            ("ORCHESTRATOR_THREAD_ID", orchestrator_thread_id.as_str()),
            ("PHASE_DOC", phase_doc.as_str()),
            ("IMPLEMENT_MD", implement_md.as_str()),
            ("TURN_DOC", turn_doc.as_str()),
            (
                "BRANCH_WORKTREE_INSTRUCTIONS",
                branch_worktree_instructions.as_str(),
            ),
        ],
    );

    Ok(PlannedFile {
        target,
        content: ensure_trailing_newline(content),
    })
}

fn render_review_prompt(dirtyloops_root: &Path, loop_dir: &Path) -> Result<PlannedFile> {
    let target = loop_dir.join("prompts/review-thread.md");
    let existing = fs::read_to_string(&target).unwrap_or_default();
    let template = read_to_string(
        &dirtyloops_root
            .join("templates/workflows/orchestrator-callback/review-thread-prompt.md.template"),
    )?;
    let phase_issue_id =
        find_prompt_issue(&existing, "review").unwrap_or_else(|| "{{PHASE_ISSUE_ID}}".to_string());
    let orchestrator_thread_id =
        find_callback_target(&existing).unwrap_or_else(|| "THREAD_ORCHESTRATOR_ID".to_string());
    let phase_doc =
        find_bullet_value(&existing, &["Phase doc"]).unwrap_or_else(|| "{{PHASE_DOC}}".to_string());
    let turn_doc =
        find_bullet_value(&existing, &["Turn doc"]).unwrap_or_else(|| "{{TURN_DOC}}".to_string());
    let pr_url_or_id =
        find_bullet_value(&existing, &["PR"]).unwrap_or_else(|| "{{PR_URL_OR_ID}}".to_string());
    let branch_or_commit = find_bullet_value(&existing, &["Branch/commit"])
        .unwrap_or_else(|| "{{BRANCH_OR_COMMIT}}".to_string());
    let quality_gates = find_bullet_value(&existing, &["Required gates"])
        .unwrap_or_else(|| "{{QUALITY_GATES}}".to_string());

    let content = render_template(
        &template,
        &[
            ("PHASE_ISSUE_ID", phase_issue_id.as_str()),
            ("ORCHESTRATOR_THREAD_ID", orchestrator_thread_id.as_str()),
            ("PHASE_DOC", phase_doc.as_str()),
            ("TURN_DOC", turn_doc.as_str()),
            ("PR_URL_OR_ID", pr_url_or_id.as_str()),
            ("BRANCH_OR_COMMIT", branch_or_commit.as_str()),
            ("QUALITY_GATES", quality_gates.as_str()),
        ],
    );

    Ok(PlannedFile {
        target,
        content: ensure_trailing_newline(content),
    })
}

fn apply_plan(planned: Vec<PlannedFile>, write_files: bool) -> Result<Vec<UpgradeAction>> {
    let mut actions = Vec::new();

    for file in planned {
        let status = match fs::read_to_string(&file.target) {
            Ok(existing) if existing == file.content => ActionStatus::Unchanged,
            Ok(_) => ActionStatus::Updated,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => ActionStatus::Created,
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", file.target.display()));
            }
        };

        if write_files && status != ActionStatus::Unchanged {
            if let Some(parent) = file.target.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            fs::write(&file.target, file.content)
                .with_context(|| format!("writing {}", file.target.display()))?;
        }

        actions.push(UpgradeAction {
            target: file.target,
            status,
        });
    }

    Ok(actions)
}

fn print_report(report: &UpgradeReport, preview: bool) {
    println!("dirtyloops source: {}", report.dirtyloops_root.display());
    println!("loop: {}", report.loop_dir.display());
    println!("workflow: {}", report.workflow.as_str());

    for action in &report.actions {
        let label = match (&action.status, preview) {
            (ActionStatus::Created, true) => "would create",
            (ActionStatus::Updated, true) => "would update",
            (ActionStatus::Created, false) => "created",
            (ActionStatus::Updated, false) => "updated",
            (ActionStatus::Unchanged, _) => "unchanged",
        };
        let display_path = action
            .target
            .strip_prefix(&report.loop_dir)
            .unwrap_or(&action.target);
        println!("{label}: {}", display_path.display());
    }

    println!(
        "summary: {} changed, {} unchanged",
        report.changed_count(),
        report.unchanged_count()
    );
}

fn resolve_dirtyloops_root(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        let path = absolute_path(path)?;
        validate_dirtyloops_root(&path)?;
        return Ok(path);
    }

    let mut candidates = Vec::new();
    if let Some(path) = env::var_os("DIRTYLOOPS_ROOT") {
        candidates.push(PathBuf::from(path));
    }
    if let Some(home) = env::var_os("HOME") {
        let home = PathBuf::from(home);
        candidates.push(home.join(".agents/skills/dirtyloops"));
        candidates.push(home.join("dev/agents/skills/dirtyloops"));
    }
    if let Ok(current_dir) = env::current_dir() {
        candidates.push(current_dir.join("skills/dirtyloops"));
        candidates.push(current_dir.join("../agents/skills/dirtyloops"));
    }

    for candidate in candidates {
        let candidate = absolute_path(&candidate)?;
        if validate_dirtyloops_root(&candidate).is_ok() {
            return Ok(candidate);
        }
    }

    bail!("could not find dirtyloops skill root; pass --dirtyloops-root or set DIRTYLOOPS_ROOT");
}

fn validate_dirtyloops_root(path: &Path) -> Result<()> {
    let required = [
        path.join("SKILL.md"),
        path.join("templates/common/run-loop.md.template"),
        path.join("templates/workflows/single-thread-subagent/run-loop-addendum.md.template"),
        path.join("templates/workflows/orchestrator-callback/run-loop-addendum.md.template"),
        path.join("schemas"),
    ];

    for item in required {
        if !item.exists() {
            bail!("dirtyloops root is missing {}", item.display());
        }
    }

    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(env::current_dir()
            .context("resolving current directory")?
            .join(path))
    }
}

fn read_to_string(path: &Path) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

fn render_template(template: &str, replacements: &[(&str, &str)]) -> String {
    let mut rendered = template.to_string();
    for (key, value) in replacements {
        rendered = rendered.replace(&format!("{{{{{key}}}}}"), value);
    }
    rendered
}

fn ensure_trailing_newline(mut content: String) -> String {
    if !content.ends_with('\n') {
        content.push('\n');
    }
    content
}

fn find_workflow(raw: &str) -> Option<String> {
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Workflow:") {
            if let Some(value) = first_backticked(trimmed) {
                return Some(value);
            }
        }
    }
    None
}

fn find_epic_id(raw: &str) -> Option<String> {
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.contains("Beads epic") || trimmed.starts_with("Canonical tracker:") {
            if let Some(value) = first_backticked(trimmed) {
                return Some(value);
            }
        }
    }
    None
}

fn find_run_loop_title(raw: &str) -> Option<String> {
    raw.lines()
        .find_map(|line| line.trim().strip_prefix("# Run Loop: "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn find_implementation_title(raw: &str) -> Option<String> {
    raw.lines()
        .find_map(|line| line.trim().strip_prefix("# "))
        .map(|title| {
            title
                .strip_suffix(" Implementation Loop")
                .unwrap_or(title)
                .trim()
                .to_string()
        })
        .filter(|value| !value.is_empty())
}

fn section_after_heading(raw: &str, heading: &str) -> Option<String> {
    let start = raw.find(heading)?;
    let after_heading = &raw[start + heading.len()..];
    Some(after_heading.trim().to_string())
}

fn default_run_prompt(
    stream_name: &str,
    workflow: Workflow,
    epic_id: &str,
    stream_slug: &str,
) -> String {
    format!(
        "Run the {stream_name} dirtyloop with workflow `{}`. Use Beads epic `{epic_id}` as canonical. Read `docs/implementation/{stream_slug}/IMPLEMENT.md` and `docs/implementation/{stream_slug}/loop-state.md`, select one next ready Beads child issue, update the existing Markdown turn doc, update Beads first, then update `loop-state.md`. Continue to the next ready phase unless the epic is complete, blocked, interrupted, or review/CI is unresolved.",
        workflow.as_str()
    )
}

fn titleize_slug(slug: &str) -> String {
    slug.split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn find_prompt_issue(raw: &str, role: &str) -> Option<String> {
    let prefix = format!("You are the {role} thread for Beads issue ");
    raw.lines()
        .find_map(|line| line.trim().strip_prefix(&prefix))
        .and_then(first_backticked)
}

fn find_callback_target(raw: &str) -> Option<String> {
    let mut lines = raw.lines();
    while let Some(line) = lines.next() {
        if line.trim() != "Callback target:" {
            continue;
        }
        for candidate in lines.by_ref() {
            let trimmed = candidate.trim();
            if trimmed.is_empty() {
                continue;
            }
            return first_backticked(trimmed).or_else(|| Some(trimmed.to_string()));
        }
    }
    None
}

fn find_bullet_value(raw: &str, labels: &[&str]) -> Option<String> {
    for line in raw.lines() {
        let trimmed = line.trim();
        let Some(bullet) = trimmed.strip_prefix("- ") else {
            continue;
        };
        for label in labels {
            let Some(value) = bullet.strip_prefix(&format!("{label}:")) else {
                continue;
            };
            let value = trim_inline_code(value.trim());
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn first_backticked(raw: &str) -> Option<String> {
    let start = raw.find('`')?;
    let rest = &raw[start + 1..];
    let end = rest.find('`')?;
    Some(rest[..end].to_string())
}

fn trim_inline_code(raw: &str) -> &str {
    raw.strip_prefix('`')
        .and_then(|value| value.strip_suffix('`'))
        .unwrap_or(raw)
}
