//! Canonical SSH target resolution and fixed argument construction.
//!
//! Every deployment/enrollment operation crosses this seam.  User input is
//! resolved once with `ssh -G`; callers then use the typed result for
//! host-key observation, managed known-host lookup, and SSH execution.  No
//! operation accepts a free-form shell fragment or an option-like target.

use anyhow::{bail, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::path::Path;
use std::process::Command;

/// The effective SSH connection selected by the user's SSH configuration.
///
/// `input` is retained for diagnostics and plan display only.  Execution must
/// use [`Self::ssh_args`] rather than passing it back as an opaque argument.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalSshTarget {
    pub input: String,
    /// OpenSSH `hostname` (the resolved HostName, not the configured alias).
    pub host_name: String,
    pub port: u16,
    pub user: String,
    /// OpenSSH `hostkeyalias`, or the effective host name when unset.
    pub host_key_alias: String,
    /// The effective `ProxyJump` chain.  Arbitrary `ProxyCommand` values are
    /// rejected because they would make the execution seam an unreviewed
    /// command interpreter.
    pub proxy_jump: Option<String>,
}

impl CanonicalSshTarget {
    pub fn resolve(input: impl Into<String>) -> Result<Self> {
        let input = input.into();
        validate_target_input(&input)?;
        let (base, explicit_port) = split_explicit_port(&input)?;
        let mut command = Command::new("ssh");
        command.arg("-G");
        if let Some(port) = explicit_port {
            command.args(["-p", &port.to_string()]);
        }
        let output = command
            .arg(&base)
            .output()
            .context("resolving SSH target with ssh -G")?;
        if !output.status.success() {
            bail!("ssh -G could not resolve the requested target");
        }
        let mut target = Self::from_ssh_config(&base, &String::from_utf8_lossy(&output.stdout))?;
        if let Some(port) = explicit_port {
            target.port = port;
        }
        target.input = input;
        Ok(target)
    }

    /// Parse `ssh -G` output without spawning a process.  This is the
    /// deterministic test seam and is also useful for callers that already
    /// performed resolution under a controlled SSH binary.
    pub fn from_ssh_config(input: &str, output: &str) -> Result<Self> {
        validate_target_input(input)?;
        let mut host_name = None;
        let mut port = None;
        let mut user = None;
        let mut host_key_alias = None;
        let mut proxy_jump = None;
        let mut proxy_command = None;

        for line in output.lines() {
            let mut fields = line.splitn(2, char::is_whitespace);
            let Some(key) = fields.next() else {
                continue;
            };
            let value = fields.next().unwrap_or("").trim();
            match key.to_ascii_lowercase().as_str() {
                "hostname" => host_name = Some(value.to_string()),
                "port" => {
                    port = Some(
                        value
                            .parse::<u16>()
                            .context("ssh -G returned an invalid port")?,
                    )
                }
                "user" => user = Some(value.to_string()),
                "hostkeyalias" => host_key_alias = Some(value.to_string()),
                "proxyjump" => {
                    if !value.is_empty() && !value.eq_ignore_ascii_case("none") {
                        proxy_jump = Some(value.to_string());
                    }
                }
                "proxycommand" if !value.is_empty() && !value.eq_ignore_ascii_case("none") => {
                    proxy_command = Some(value.to_string());
                }
                _ => {}
            }
        }

        if proxy_command.is_some() {
            bail!("SSH target resolves to an unsupported ProxyCommand; use a reviewed ProxyJump");
        }
        let host_name = host_name.context("ssh -G omitted HostName")?;
        let port = port.context("ssh -G omitted Port")?;
        let user = user.context("ssh -G omitted User")?;
        let host_key_alias = host_key_alias
            .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("none"))
            .unwrap_or_else(|| host_name.clone());
        validate_host_component(&host_name, "resolved HostName")?;
        validate_user(&user)?;
        validate_host_key_alias(&host_key_alias)?;
        if port == 0 {
            bail!("resolved SSH Port must be non-zero");
        }
        if let Some(proxy) = &proxy_jump {
            validate_proxy_jump(proxy)?;
        }

        Ok(Self {
            input: input.to_string(),
            host_name,
            port,
            user,
            host_key_alias,
            proxy_jump,
        })
    }

    pub fn destination(&self) -> String {
        format!("{}@{}", self.user, self.host_name)
    }

    /// The managed known-host index key.  HostKeyAlias is deliberately part
    /// of the key so aliases and configured host names cannot cross-trust.
    pub fn host_key_name(&self) -> String {
        if self.host_key_alias == self.host_name && self.port != 22 {
            format!("[{}]:{}", self.host_key_alias, self.port)
        } else {
            self.host_key_alias.clone()
        }
    }

    /// Fixed OpenSSH arguments for one operation.  The target is represented
    /// as `-p <port> user@host`; `user@host:port` is never emitted because it
    /// is not valid OpenSSH syntax.
    pub fn ssh_args(
        &self,
        known_hosts: &Path,
        key_path: Option<&Path>,
        batch: bool,
    ) -> Vec<String> {
        let mut args = vec![
            "-o".to_string(),
            "StrictHostKeyChecking=yes".to_string(),
            "-o".to_string(),
            format!("UserKnownHostsFile={}", known_hosts.display()),
            "-o".to_string(),
            "ConnectTimeout=10".to_string(),
            "-o".to_string(),
            format!("HostKeyAlias={}", self.host_key_alias),
            "-p".to_string(),
            self.port.to_string(),
        ];
        if batch {
            args.splice(0..0, ["-o".to_string(), "BatchMode=yes".to_string()]);
        }
        if let Some(proxy_jump) = &self.proxy_jump {
            args.extend(["-o".to_string(), format!("ProxyJump={proxy_jump}")]);
        }
        if let Some(key_path) = key_path {
            args.extend([
                "-i".to_string(),
                key_path.display().to_string(),
                "-o".to_string(),
                "IdentitiesOnly=yes".to_string(),
            ]);
        }
        args.push(self.destination());
        args
    }

    /// Fixed `ssh-keyscan` arguments for the effective endpoint.  Keyscan
    /// does not execute ProxyCommand; the resolved HostName/Port are used and
    /// the managed index still binds the observation to HostKeyAlias.
    pub fn keyscan_args(&self) -> Vec<String> {
        vec![
            "-T".to_string(),
            "10".to_string(),
            "-p".to_string(),
            self.port.to_string(),
            self.host_name.clone(),
        ]
    }
}

fn split_explicit_port(value: &str) -> Result<(String, Option<u16>)> {
    // Accept the human-friendly `user@host:port` form, but never emit it to
    // OpenSSH. Bracketed IPv6 targets are handled without treating their
    // internal colons as a port separator.
    if let Some((prefix, port)) = value.rsplit_once(':') {
        let bracketed_ipv6 = prefix.ends_with(']') && prefix.contains('[');
        let simple_host = !prefix.contains(':');
        if (simple_host || bracketed_ipv6)
            && port.chars().all(|character| character.is_ascii_digit())
        {
            let port = port.parse::<u16>().context("SSH target port is invalid")?;
            if port == 0 {
                bail!("SSH target port must be non-zero");
            }
            if prefix.is_empty() {
                bail!("SSH target host is empty");
            }
            return Ok((prefix.to_string(), Some(port)));
        }
    }
    Ok((value.to_string(), None))
}

pub fn canonical_known_hosts_line(target: &CanonicalSshTarget, line: &str) -> Result<String> {
    let mut fields = line.splitn(2, char::is_whitespace);
    let _observed_host = fields.next().unwrap_or("");
    let rest = fields.next().unwrap_or("").trim_start();
    if _observed_host.is_empty() || rest.is_empty() {
        bail!("ssh-keyscan returned a malformed host-key line");
    }
    Ok(format!("{} {}", target.host_key_name(), rest))
}

/// Fingerprint the actual OpenSSH public-key payload, not the host name or
/// comment text. The deterministic lower-case hex form is easy to compare in
/// a managed sidecar and in operator prompts.
pub fn host_key_fingerprint(line: &str) -> Result<String> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 3 {
        bail!("ssh-keyscan returned a malformed host-key line");
    }
    let key = base64::engine::general_purpose::STANDARD
        .decode(fields[2])
        .context("ssh-keyscan returned an invalid public-key payload")?;
    if key.is_empty() {
        bail!("ssh-keyscan returned an empty public-key payload");
    }
    Ok(format!("sha256:{}", hex::encode(sha2::Sha256::digest(key))))
}

pub fn validate_target_input(value: &str) -> Result<()> {
    if value.trim().is_empty()
        || value.len() > 255
        || value.starts_with('-')
        || value.contains('=')
        || value.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(
                    character,
                    '\'' | '"' | '`' | ';' | '&' | '|' | '$' | '<' | '>'
                )
        })
    {
        bail!("SSH target is not a safe alias or user@host target");
    }
    Ok(())
}

fn validate_host_component(value: &str, field: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 255
        || value.starts_with('-')
        || value.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(
                    character,
                    '\'' | '"' | '`' | ';' | '&' | '|' | '$' | '<' | '>'
                )
        })
    {
        bail!("{field} is invalid");
    }
    Ok(())
}

fn validate_user(value: &str) -> Result<()> {
    validate_host_component(value, "resolved User")
}

fn validate_host_key_alias(value: &str) -> Result<()> {
    validate_host_component(value, "resolved HostKeyAlias")
}

fn validate_proxy_jump(value: &str) -> Result<()> {
    if value.is_empty()
        || value.starts_with('-')
        || value.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(
                    character,
                    '\'' | '"' | '`' | ';' | '&' | '|' | '$' | '<' | '>'
                )
        })
    {
        bail!("resolved ProxyJump is invalid");
    }
    Ok(())
}

/// Validate either an IP literal or a CIDR.  Kept here so SSH/listener
/// callers can share the same strict input rules without a third-party CIDR
/// dependency.
pub fn validate_cidr(value: &str) -> Result<()> {
    let Some((network, prefix)) = value.split_once('/') else {
        value
            .parse::<std::net::IpAddr>()
            .with_context(|| format!("invalid CIDR address {value}"))?;
        return Ok(());
    };
    let address = network
        .parse::<std::net::IpAddr>()
        .with_context(|| format!("invalid CIDR network {network}"))?;
    let prefix = prefix
        .parse::<u8>()
        .with_context(|| format!("invalid CIDR prefix {prefix}"))?;
    let max = match address {
        std::net::IpAddr::V4(_) => 32,
        std::net::IpAddr::V6(_) => 128,
    };
    if prefix > max {
        bail!("CIDR prefix {prefix} exceeds address width {max}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SSH_G: &str = "hostname resolved.example\nport 2222\nuser deploy\nhostkeyalias managed-example\nproxyjump bastion\nproxycommand none\n";

    #[test]
    fn ssh_g_resolves_all_effective_connection_facts() {
        let target = CanonicalSshTarget::from_ssh_config("deploy@example", SSH_G).unwrap();
        assert_eq!(target.host_name, "resolved.example");
        assert_eq!(target.port, 2222);
        assert_eq!(target.user, "deploy");
        assert_eq!(target.host_key_name(), "managed-example");
        assert_eq!(target.proxy_jump.as_deref(), Some("bastion"));
        let args = target.ssh_args(Path::new("known_hosts"), None, true);
        assert!(args.windows(2).any(|pair| pair == ["-p", "2222"]));
        assert!(args.iter().any(|arg| arg == "deploy@resolved.example"));
        assert!(!args.iter().any(|arg| arg == "deploy@resolved.example:2222"));
    }

    #[test]
    fn target_rejects_options_and_shell_fragments() {
        assert!(validate_target_input("--proxy-command=bad").is_err());
        assert!(validate_target_input("host;touch").is_err());
        assert!(CanonicalSshTarget::from_ssh_config(
            "host",
            "hostname host\nport 22\nuser user\nproxycommand sh -c bad\n"
        )
        .is_err());
    }

    #[test]
    fn host_fingerprint_hashes_key_payload_not_host_text() {
        let first = host_key_fingerprint("host ssh-ed25519 AQID").unwrap();
        let second = host_key_fingerprint("other ssh-ed25519 AQID comment").unwrap();
        assert_eq!(first, second);
        assert!(host_key_fingerprint("host ssh-ed25519 not-base64!").is_err());
    }

    #[test]
    fn cidr_validation_covers_ipv4_ipv6_and_bad_prefixes() {
        assert!(validate_cidr("127.0.0.1/32").is_ok());
        assert!(validate_cidr("fd7a:115c:a1e0::/48").is_ok());
        assert!(validate_cidr("127.0.0.1/33").is_err());
        assert!(validate_cidr("not-an-ip/24").is_err());
    }
}
