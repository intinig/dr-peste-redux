use anyhow::{Context, Result};

#[derive(Clone)]
pub struct Config {
    pub discord_token: String,
    pub guild_id: u64,
    pub poll_interval_mins: u64,
    pub min_volume: f64,
    pub poesessid: Option<String>,
    pub proxy: Option<crate::trade::session::ProxyConfig>,
    pub session_ttl_mins: u64,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let discord_token = get("DISCORD_TOKEN")
            .filter(|s| !s.is_empty())
            .context("DISCORD_TOKEN must be set")?;
        let guild_id = get("GUILD_ID")
            .context("GUILD_ID must be set")?
            .parse::<u64>()
            .context("GUILD_ID must be a valid u64")?;
        let poll_interval_mins = match get("POLL_INTERVAL_MINS") {
            Some(v) => v
                .parse::<u64>()
                .context("POLL_INTERVAL_MINS must be a u64")?,
            None => 30,
        };
        if poll_interval_mins == 0 {
            anyhow::bail!("POLL_INTERVAL_MINS must be at least 1 (got 0)");
        }
        let min_volume = match get("MIN_VOLUME") {
            Some(v) => v.parse::<f64>().context("MIN_VOLUME must be a number")?,
            None => 0.0,
        };
        let poesessid = get("POESESSID").filter(|s| !s.is_empty());

        let session_ttl_mins = match get("SESSION_TTL_MINS") {
            Some(v) => v.parse::<u64>().context("SESSION_TTL_MINS must be a u64")?,
            None => 180,
        };

        let proxy = match (
            get("PROXY_GATEWAY").filter(|s| !s.is_empty()),
            get("PROXY_USER").filter(|s| !s.is_empty()),
            get("PROXY_PASS").filter(|s| !s.is_empty()),
        ) {
            (Some(gateway), Some(user), Some(pass)) => {
                let country = get("PROXY_COUNTRY")
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "us".to_string());
                let lifetime_mins = match get("PROXY_SESSION_LIFETIME_MINS") {
                    Some(v) => v
                        .parse::<u64>()
                        .context("PROXY_SESSION_LIFETIME_MINS must be a u64")?,
                    None => 30,
                };
                Some(crate::trade::session::ProxyConfig {
                    gateway,
                    user,
                    pass,
                    country,
                    lifetime_mins,
                })
            }
            _ => None,
        };

        Ok(Self {
            discord_token,
            guild_id,
            poll_interval_mins,
            min_volume,
            poesessid,
            proxy,
            session_ttl_mins,
        })
    }
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("discord_token", &"[REDACTED]")
            .field("guild_id", &self.guild_id)
            .field("poll_interval_mins", &self.poll_interval_mins)
            .field("min_volume", &self.min_volume)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn parses_full_config() {
        let cfg = Config::from_lookup(lookup(&[
            ("DISCORD_TOKEN", "abc"),
            ("GUILD_ID", "123"),
            ("POLL_INTERVAL_MINS", "15"),
            ("MIN_VOLUME", "5.5"),
        ]))
        .unwrap();
        assert_eq!(cfg.discord_token, "abc");
        assert_eq!(cfg.guild_id, 123);
        assert_eq!(cfg.poll_interval_mins, 15);
        assert_eq!(cfg.min_volume, 5.5);
    }

    #[test]
    fn applies_defaults() {
        let cfg =
            Config::from_lookup(lookup(&[("DISCORD_TOKEN", "abc"), ("GUILD_ID", "1")])).unwrap();
        assert_eq!(cfg.poll_interval_mins, 30);
        assert_eq!(cfg.min_volume, 0.0);
    }

    #[test]
    fn missing_token_errors() {
        assert!(Config::from_lookup(lookup(&[("GUILD_ID", "1")])).is_err());
    }

    #[test]
    fn non_numeric_guild_errors() {
        assert!(Config::from_lookup(lookup(&[("DISCORD_TOKEN", "a"), ("GUILD_ID", "x")])).is_err());
    }

    #[test]
    fn rejects_zero_poll_interval() {
        assert!(Config::from_lookup(lookup(&[
            ("DISCORD_TOKEN", "a"),
            ("GUILD_ID", "1"),
            ("POLL_INTERVAL_MINS", "0"),
        ]))
        .is_err());
    }

    #[test]
    fn reads_optional_poe_sessid() {
        let cfg = Config::from_lookup(|k| match k {
            "DISCORD_TOKEN" => Some("t".into()),
            "GUILD_ID" => Some("1".into()),
            "POESESSID" => Some("abc".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(cfg.poesessid.as_deref(), Some("abc"));
        let cfg2 = Config::from_lookup(|k| match k {
            "DISCORD_TOKEN" => Some("t".into()),
            "GUILD_ID" => Some("1".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(cfg2.poesessid, None);
    }

    #[test]
    fn parses_proxy_and_ttl_when_all_present() {
        let cfg = Config::from_lookup(|k| match k {
            "DISCORD_TOKEN" => Some("t".into()),
            "GUILD_ID" => Some("1".into()),
            "PROXY_GATEWAY" => Some("geo.iproyal.com:12321".into()),
            "PROXY_USER" => Some("u".into()),
            "PROXY_PASS" => Some("p".into()),
            "PROXY_COUNTRY" => Some("de".into()),
            "SESSION_TTL_MINS" => Some("60".into()),
            _ => None,
        })
        .unwrap();
        let proxy = cfg.proxy.expect("proxy configured");
        assert_eq!(proxy.gateway, "geo.iproyal.com:12321");
        assert_eq!(proxy.country, "de");
        assert_eq!(cfg.session_ttl_mins, 60);
    }

    #[test]
    fn proxy_is_none_when_incomplete() {
        let cfg = Config::from_lookup(|k| match k {
            "DISCORD_TOKEN" => Some("t".into()),
            "GUILD_ID" => Some("1".into()),
            "PROXY_GATEWAY" => Some("geo.iproyal.com:12321".into()),
            // missing PROXY_USER / PROXY_PASS
            _ => None,
        })
        .unwrap();
        assert!(cfg.proxy.is_none());
    }

    #[test]
    fn reads_poesessid_env() {
        let cfg = Config::from_lookup(|k| match k {
            "DISCORD_TOKEN" => Some("t".into()),
            "GUILD_ID" => Some("1".into()),
            "POESESSID" => Some("abc".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(cfg.poesessid.as_deref(), Some("abc"));
    }
}
