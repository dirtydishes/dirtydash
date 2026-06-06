use std::process::Command;

use anyhow::{Context, Result};

use crate::app_paths::AppPaths;
use crate::cli::{RemoteCommand, RemoteSubcommand};
use crate::config::{Config, RemoteConfig};
use crate::db::Database;

pub fn run(paths: &AppPaths, config: &mut Config, args: RemoteCommand) -> Result<()> {
    let db = Database::open(&paths.db_path)?;
    db.migrate()?;

    match args.command {
        RemoteSubcommand::Add(add) => {
            let mut remote = RemoteConfig {
                name: add.name.clone(),
                ssh_target: add.ssh_target.clone(),
                source_roots: Vec::new(),
            };
            let mut remote_config = Config::default();
            remote_config.merge_cli_source_roots(&add.source_root)?;
            remote.source_roots = remote_config.source_roots;

            config
                .remotes
                .retain(|existing| existing.name != remote.name);
            config.remotes.push(remote.clone());
            config.save(&paths.config_path)?;

            let roots_json = serde_json::to_string(&remote.source_roots)?;
            db.add_remote(&remote.name, &remote.ssh_target, &roots_json)?;
            println!("added remote {} ({})", remote.name, remote.ssh_target);
        }
        RemoteSubcommand::List(list) => {
            let rows = db.list_remotes()?;
            if list.json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                println!("name\tssh_target\tfiles\tlast_sync\tlast_error");
                for row in rows {
                    println!(
                        "{}\t{}\t{}\t{}\t{}",
                        row.name,
                        row.ssh_target,
                        row.last_file_count,
                        row.last_sync_at.unwrap_or_else(|| "-".to_string()),
                        row.last_error.unwrap_or_else(|| "-".to_string())
                    );
                }
            }
        }
        RemoteSubcommand::Remove(remove) => {
            config.remotes.retain(|remote| remote.name != remove.name);
            config.save(&paths.config_path)?;
            db.remove_remote(&remove.name)?;
            println!("removed remote {}", remove.name);
        }
        RemoteSubcommand::Sync(sync) => {
            let selected: Vec<_> = config
                .remotes
                .iter()
                .filter(|remote| sync.name.as_ref().is_none_or(|name| name == &remote.name))
                .cloned()
                .collect();

            if selected.is_empty() {
                println!("no matching remotes configured");
                return Ok(());
            }

            for remote in selected {
                match sync_remote(&remote) {
                    Ok(file_count) => {
                        db.update_remote_sync(&remote.name, file_count, None)?;
                        println!("{}: discovered {} remote files", remote.name, file_count);
                    }
                    Err(error) => {
                        db.update_remote_sync(&remote.name, 0, Some(&error.to_string()))?;
                        println!("{}: sync failed: {error}", remote.name);
                    }
                }
            }
        }
    }

    Ok(())
}

fn sync_remote(remote: &RemoteConfig) -> Result<u64> {
    let paths = if remote.source_roots.is_empty() {
        vec![
            "~/.config/claude/projects".to_string(),
            "~/.claude/projects".to_string(),
            "~/.codex/sessions".to_string(),
            "~/.local/share/opencode/storage/message".to_string(),
            "~/.pi/agent/sessions".to_string(),
        ]
    } else {
        remote
            .source_roots
            .iter()
            .map(|root| root.path.display().to_string())
            .collect()
    };

    let remote_script = build_read_only_find_script(&paths);
    let output = Command::new("ssh")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=8")
        .arg(&remote.ssh_target)
        .arg(remote_script)
        .output()
        .with_context(|| format!("running ssh discovery for {}", remote.name))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        anyhow::bail!(if stderr.is_empty() {
            format!("ssh exited with {}", output.status)
        } else {
            stderr
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let count = stdout
        .lines()
        .filter_map(|line| line.trim().parse::<u64>().ok())
        .sum();
    Ok(count)
}

fn build_read_only_find_script(paths: &[String]) -> String {
    let quoted_paths = paths
        .iter()
        .map(|path| shell_quote(path))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "set -eu; for p in {quoted_paths}; do expanded=$(eval printf '%s' \"$p\"); if [ -d \"$expanded\" ]; then find \"$expanded\" -type f \\( -name '*.json' -o -name '*.jsonl' \\) | wc -l; fi; done"
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
