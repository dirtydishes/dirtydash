//! Hub listener access modes and the explicit Tailscale/public trust policy.
//!
//! This module deliberately contains no credentials.  It is used by both the
//! deployment planner and the runnable Hub listener so a plan cannot silently
//! select a less trusted serving mode than the process that consumes it.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ListenerAccessMode {
    /// The private-by-default entry point.  Tailscale owns the HTTPS boundary.
    #[default]
    TailscaleServe,
    /// HTTPS is terminated by an explicitly configured public reverse proxy.
    PublicHttps,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TailscaleServeState {
    NotConfigured,
    #[default]
    ConsentRequired,
    Enabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicTrustConfig {
    /// The Hub always performs fallback administrator authentication in this
    /// mode.  This field is descriptive and makes the policy inspectable.
    #[serde(default = "fallback_admin_auth")]
    pub administrator_auth: String,
    pub trusted_proxy: Option<PublicTrustedProxy>,
    #[serde(default = "default_secure_cookies")]
    pub secure_cookies: bool,
}

fn fallback_admin_auth() -> String {
    "fallback-admin-session".to_string()
}

fn default_secure_cookies() -> bool {
    true
}

impl Default for PublicTrustConfig {
    fn default() -> Self {
        Self {
            administrator_auth: fallback_admin_auth(),
            trusted_proxy: None,
            secure_cookies: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicTrustedProxy {
    pub identity_header: String,
    pub provenance_header: String,
    pub provenance_value: String,
    pub source_cidrs: Vec<String>,
}

impl PublicTrustedProxy {
    pub fn validate(&self) -> Result<()> {
        if self.identity_header.trim().is_empty()
            || self.provenance_header.trim().is_empty()
            || self.provenance_value.trim().is_empty()
            || self.source_cidrs.is_empty()
        {
            bail!("trusted public proxy configuration must be complete and fail closed");
        }
        for cidr in &self.source_cidrs {
            crate::ssh::validate_cidr(cidr)?;
        }
        Ok(())
    }
}

impl PublicTrustConfig {
    pub fn validate(&self) -> Result<()> {
        if self.administrator_auth != "fallback-admin-session" {
            bail!("public listeners require fallback administrator authentication");
        }
        if !self.secure_cookies {
            bail!("public listeners require secure cookies");
        }
        if let Some(proxy) = &self.trusted_proxy {
            proxy.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListenerPlan {
    pub access_mode: ListenerAccessMode,
    pub tailscale_state: TailscaleServeState,
    pub public: Option<PublicTrustConfig>,
    pub local_port: u16,
}

impl Default for ListenerPlan {
    fn default() -> Self {
        Self::tailscale_default(4599)
    }
}

impl ListenerPlan {
    pub fn tailscale_default(local_port: u16) -> Self {
        Self {
            access_mode: ListenerAccessMode::TailscaleServe,
            tailscale_state: TailscaleServeState::ConsentRequired,
            public: None,
            local_port,
        }
    }

    pub fn public_https(local_port: u16, public: PublicTrustConfig) -> Result<Self> {
        public.validate()?;
        Ok(Self {
            access_mode: ListenerAccessMode::PublicHttps,
            tailscale_state: TailscaleServeState::NotConfigured,
            public: Some(public),
            local_port,
        })
    }

    pub fn validate(&self) -> Result<()> {
        if self.local_port == 0 {
            bail!("Hub listener port must be non-zero");
        }
        match self.access_mode {
            ListenerAccessMode::TailscaleServe => {
                if self.public.is_some() {
                    bail!("Tailscale Serve and public proxy trust are mutually exclusive");
                }
            }
            ListenerAccessMode::PublicHttps => {
                if self.tailscale_state != TailscaleServeState::NotConfigured {
                    bail!("public HTTPS mode cannot carry Tailscale consent state");
                }
                self.public
                    .as_ref()
                    .context("public HTTPS mode requires fallback trust configuration")?
                    .validate()?;
            }
        }
        Ok(())
    }

    /// Validate the bind address for a concrete process.  Private Tailscale
    /// mode is safe on loopback because Tailscale Serve supplies the external
    /// transport/trust boundary; a non-loopback bind requires an explicit
    /// trusted-proxy/public mode instead.
    pub fn validate_bind_host(&self, host: &str) -> Result<()> {
        if host.trim().is_empty()
            || host
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
        {
            bail!("Hub bind host is invalid");
        }
        if self.access_mode == ListenerAccessMode::TailscaleServe
            && !matches!(host, "127.0.0.1" | "localhost" | "::1")
        {
            bail!("private Tailscale Serve mode must bind loopback-only");
        }
        Ok(())
    }

    /// A non-secret configuration fragment consumed by the Hub runtime.  It
    /// intentionally omits setup/password/token material; those are handled
    /// by the existing memory/request authentication seams.
    pub fn render_runtime_toml(&self) -> Result<String> {
        self.render_runtime_toml_with_collector(None, None)
    }

    /// Render the complete runtime fragment used by a hosted Collector. The
    /// local-only form remains loopback-bound, while hosted enrollment passes
    /// the Hub's explicitly configured canonical URL and Machine ID.
    pub fn render_runtime_toml_with_collector(
        &self,
        canonical_hub_url: Option<&str>,
        machine_id: Option<&str>,
    ) -> Result<String> {
        self.render_runtime_toml_with_collector_details(canonical_hub_url, machine_id, None)
    }

    pub fn render_runtime_toml_with_collector_details(
        &self,
        canonical_hub_url: Option<&str>,
        machine_id: Option<&str>,
        update_target: Option<&str>,
    ) -> Result<String> {
        self.validate()?;
        if let Some(url) = canonical_hub_url {
            let parsed = reqwest::Url::parse(url)
                .map_err(|_| anyhow::anyhow!("canonical Hub URL is invalid"))?;
            if !matches!(parsed.scheme(), "https" | "http")
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.query().is_some()
                || parsed.fragment().is_some()
                || parsed.path() != "/"
            {
                bail!("canonical Hub URL must be an origin URL");
            }
            if parsed.scheme() == "http"
                && !parsed
                    .host_str()
                    .is_some_and(|host| matches!(host, "127.0.0.1" | "localhost" | "::1"))
            {
                bail!("canonical Hub URL must use TLS unless it targets loopback");
            }
        }
        if let Some(machine_id) = machine_id {
            if machine_id.trim().is_empty()
                || !machine_id.chars().all(|character| {
                    character.is_ascii_alphanumeric()
                        || matches!(character, '-' | '_' | '.' | ':' | '@')
                })
            {
                bail!("Collector Machine ID is invalid");
            }
        }
        let mut output = format!(
            "[hub.listener]\naccess_mode = \"{}\"\ntailscale_state = \"{}\"\nlocal_port = {}\n",
            match self.access_mode {
                ListenerAccessMode::TailscaleServe => "tailscale-serve",
                ListenerAccessMode::PublicHttps => "public-https",
            },
            match self.tailscale_state {
                TailscaleServeState::NotConfigured => "not-configured",
                TailscaleServeState::ConsentRequired => "consent-required",
                TailscaleServeState::Enabled => "enabled",
            },
            self.local_port
        );
        let collector_url = canonical_hub_url
            .map(str::to_owned)
            .unwrap_or_else(|| format!("http://127.0.0.1:{}", self.local_port));
        output.push_str("\n[collector]\n");
        output.push_str(&format!("hub_url = {}\n", toml_string(&collector_url)));
        if let Some(machine_id) = machine_id {
            output.push_str(&format!("machine_id = {}\n", toml_string(machine_id)));
        }
        if let Some(update_target) = update_target {
            if update_target.trim().is_empty()
                || update_target
                    .chars()
                    .any(|character| character.is_control())
            {
                bail!("Collector update target is invalid");
            }
            output.push_str(&format!("update_target = {}\n", toml_string(update_target)));
        }
        if let Some(public) = &self.public {
            output.push_str("\n[hub.listener.public]\n");
            output.push_str("administrator_auth = \"fallback-admin-session\"\n");
            output.push_str("secure_cookies = true\n");
            if let Some(proxy) = &public.trusted_proxy {
                output.push_str("\n[hub.listener.public.trusted_proxy]\n");
                output.push_str(&format!(
                    "identity_header = {}\nprovenance_header = {}\nprovenance_value = {}\nsource_cidrs = {}\n",
                    toml_string(&proxy.identity_header),
                    toml_string(&proxy.provenance_header),
                    toml_string(&proxy.provenance_value),
                    toml_array(&proxy.source_cidrs),
                ));
            }
        }
        Ok(output)
    }

    pub fn tailscale_command(&self) -> Option<Vec<String>> {
        if self.access_mode != ListenerAccessMode::TailscaleServe {
            return None;
        }
        Some(vec![
            "tailscale".to_string(),
            "serve".to_string(),
            "--https=443".to_string(),
            format!("http://127.0.0.1:{}", self.local_port),
        ])
    }

    pub fn apply_tailscale_output(&mut self, output: &str) -> TailscaleServeState {
        if self.access_mode != ListenerAccessMode::TailscaleServe {
            return TailscaleServeState::NotConfigured;
        }
        let lower = output.to_ascii_lowercase();
        self.tailscale_state = if lower.contains("consent")
            || lower.contains("approve")
            || lower.contains("approv")
            || lower.contains("permission")
            || lower.contains("tailnet admin")
        {
            TailscaleServeState::ConsentRequired
        } else {
            TailscaleServeState::Enabled
        };
        self.tailscale_state
    }
}

fn toml_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn toml_array(values: &[String]) -> String {
    let values = values
        .iter()
        .map(|value| toml_string(value))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{values}]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tailscale_is_private_default_and_consent_is_resumable() {
        let mut plan = ListenerPlan::default();
        assert_eq!(plan.access_mode, ListenerAccessMode::TailscaleServe);
        assert_eq!(plan.tailscale_state, TailscaleServeState::ConsentRequired);
        assert_eq!(
            plan.apply_tailscale_output("tailnet admin approval required"),
            TailscaleServeState::ConsentRequired
        );
        assert_eq!(
            plan.apply_tailscale_output("Serve is running"),
            TailscaleServeState::Enabled
        );
    }

    #[test]
    fn private_tailscale_is_loopback_only_and_cidr_validation_is_fail_closed() {
        let plan = ListenerPlan::default();
        assert!(plan.validate_bind_host("127.0.0.1").is_ok());
        assert!(plan.validate_bind_host("0.0.0.0").is_err());
        let invalid = PublicTrustConfig {
            trusted_proxy: Some(PublicTrustedProxy {
                identity_header: "x-user".to_string(),
                provenance_header: "x-provenance".to_string(),
                provenance_value: "verified".to_string(),
                source_cidrs: vec!["2001:db8::/129".to_string()],
            }),
            ..PublicTrustConfig::default()
        };
        assert!(invalid.validate().is_err());
        let mixed = PublicTrustConfig {
            trusted_proxy: Some(PublicTrustedProxy {
                identity_header: "x-user".to_string(),
                provenance_header: "x-provenance".to_string(),
                provenance_value: "verified".to_string(),
                source_cidrs: vec![
                    "127.0.0.1/32".to_string(),
                    "fd7a:115c:a1e0::/48".to_string(),
                ],
            }),
            ..PublicTrustConfig::default()
        };
        assert!(mixed.validate().is_ok());
    }

    #[test]
    fn public_mode_requires_fallback_and_secure_proxy_configuration() {
        let public = PublicTrustConfig {
            trusted_proxy: Some(PublicTrustedProxy {
                identity_header: "x-user".to_string(),
                provenance_header: "x-provenance".to_string(),
                provenance_value: "verified".to_string(),
                source_cidrs: vec!["127.0.0.1/32".to_string()],
            }),
            ..PublicTrustConfig::default()
        };
        let plan = ListenerPlan::public_https(4599, public).unwrap();
        let rendered = plan.render_runtime_toml().unwrap();
        assert!(rendered.contains("fallback-admin-session"));
        assert!(rendered.contains("source_cidrs"));
        assert!(rendered.contains("[collector]"));
        assert!(!rendered.contains("password"));
        let parsed: crate::config::Config = toml::from_str(&rendered).unwrap();
        assert_eq!(
            parsed.collector.hub_url.as_deref(),
            Some("http://127.0.0.1:4599")
        );
    }
}
