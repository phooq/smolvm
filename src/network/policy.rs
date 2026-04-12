//! Network policy helpers for launch planning and virtio runtime enforcement.
//!
//! Context
//! =======
//!
//! smolvm currently has two sources of outbound policy:
//! - `allowed_cidrs`: guest may only connect to these IP ranges
//! - `dns_filter_hosts`: guest DNS may only resolve these hostnames
//!
//! The CLI already resolves `--allow-host` values to CIDRs at VM start, so the
//! runtime policy model can stay simple:
//! - CIDR checks gate TCP/UDP destination IPs
//! - hostname checks gate DNS queries
//!
//! That means the virtio runtime does not need to do live hostname-to-IP
//! policy expansion. It receives the already-resolved CIDR/IP list and the
//! original hostname allowlist as separate inputs.

use crate::dns_filter::DnsFilter;
use crate::vm::config::NetworkPolicy;
use ipnet::IpNet;
use std::net::{IpAddr, Ipv4Addr};

/// Launch-time policy inputs owned by the selected network backend.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchEgressPolicy {
    /// Allowed guest egress destination ranges.
    pub allowed_cidrs: Option<Vec<String>>,
    /// Allowed guest DNS names.
    pub dns_filter_hosts: Option<Vec<String>>,
}

/// Runtime-ready egress policy for the virtio backend.
#[derive(Debug, Clone)]
pub struct ResolvedEgressPolicy {
    allowed_cidrs: Option<Vec<IpNet>>,
    dns_filter: Option<DnsFilter>,
}

impl LaunchEgressPolicy {
    /// Whether either CIDR or hostname filtering is configured.
    pub fn is_configured(&self) -> bool {
        self.allowed_cidrs
            .as_ref()
            .is_some_and(|cidrs| !cidrs.is_empty())
            || self
                .dns_filter_hosts
                .as_ref()
                .is_some_and(|hosts| !hosts.is_empty())
    }
}

impl ResolvedEgressPolicy {
    /// Build a runtime policy from launch-time inputs.
    pub fn compile(policy: LaunchEgressPolicy, upstream_dns: Ipv4Addr) -> Result<Self, String> {
        let allowed_cidrs = policy
            .allowed_cidrs
            .map(|cidrs| {
                cidrs
                    .into_iter()
                    .map(|cidr| parse_cidr_or_ip(&cidr))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;

        let dns_filter = policy.dns_filter_hosts.and_then(|hosts| {
            if hosts.is_empty() {
                None
            } else {
                Some(DnsFilter::new(hosts, upstream_dns.to_string()))
            }
        });

        Ok(Self {
            allowed_cidrs,
            dns_filter,
        })
    }

    /// Whether the destination IP is allowed for guest egress.
    pub fn allows_ip(&self, ip: IpAddr) -> bool {
        match &self.allowed_cidrs {
            None => true,
            Some(cidrs) => cidrs.iter().any(|cidr| cidr.contains(&ip)),
        }
    }

    /// Whether DNS hostname filtering is configured.
    pub fn has_dns_filter(&self) -> bool {
        self.dns_filter.is_some()
    }

    /// Filter a raw DNS query if hostname policy is configured.
    ///
    /// Returns `Some(response)` when DNS filtering is active, otherwise `None`
    /// so callers can use an unrestricted upstream forward path.
    pub fn filter_dns_query(&self, raw_query: &[u8]) -> Option<Vec<u8>> {
        self.dns_filter
            .as_ref()
            .map(|filter| filter.handle_query(raw_query))
    }
}

/// Get the DNS server for a network policy.
pub fn get_dns_server(policy: &NetworkPolicy) -> Option<IpAddr> {
    match policy {
        NetworkPolicy::None => None,
        NetworkPolicy::Egress { dns, .. } => {
            Some(dns.unwrap_or(crate::data::network::DEFAULT_DNS_ADDR))
        }
    }
}

fn parse_cidr_or_ip(value: &str) -> Result<IpNet, String> {
    value
        .parse::<IpNet>()
        .or_else(|_| value.parse::<IpAddr>().map(IpNet::from))
        .map_err(|_| format!("invalid CIDR or IP address in egress policy: {value}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_dns_server() {
        assert!(get_dns_server(&NetworkPolicy::None).is_none());

        let dns = get_dns_server(&NetworkPolicy::Egress {
            dns: None,
            allowed_cidrs: None,
        })
        .unwrap();
        assert_eq!(dns.to_string(), crate::data::network::DEFAULT_DNS);

        let custom: IpAddr = "8.8.8.8".parse().unwrap();
        let dns = get_dns_server(&NetworkPolicy::Egress {
            dns: Some(custom),
            allowed_cidrs: None,
        })
        .unwrap();
        assert_eq!(dns.to_string(), "8.8.8.8");
    }

    #[test]
    fn test_launch_policy_is_configured() {
        assert!(!LaunchEgressPolicy::default().is_configured());
        assert!(LaunchEgressPolicy {
            allowed_cidrs: Some(vec!["10.0.0.0/8".into()]),
            dns_filter_hosts: None,
        }
        .is_configured());
        assert!(LaunchEgressPolicy {
            allowed_cidrs: None,
            dns_filter_hosts: Some(vec!["api.stripe.com".into()]),
        }
        .is_configured());
    }

    #[test]
    fn test_compile_policy_and_allow_ip() {
        let policy = ResolvedEgressPolicy::compile(
            LaunchEgressPolicy {
                allowed_cidrs: Some(vec!["10.0.0.0/8".into(), "1.1.1.1".into()]),
                dns_filter_hosts: None,
            },
            Ipv4Addr::new(1, 1, 1, 1),
        )
        .unwrap();

        assert!(policy.allows_ip("10.2.3.4".parse().unwrap()));
        assert!(policy.allows_ip("1.1.1.1".parse().unwrap()));
        assert!(!policy.allows_ip("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn test_compile_policy_rejects_invalid_cidr() {
        let err = ResolvedEgressPolicy::compile(
            LaunchEgressPolicy {
                allowed_cidrs: Some(vec!["not-a-cidr".into()]),
                dns_filter_hosts: None,
            },
            Ipv4Addr::new(1, 1, 1, 1),
        )
        .unwrap_err();

        assert!(err.contains("invalid CIDR or IP address"));
    }

    #[test]
    fn test_compile_policy_builds_dns_filter() {
        let policy = ResolvedEgressPolicy::compile(
            LaunchEgressPolicy {
                allowed_cidrs: None,
                dns_filter_hosts: Some(vec!["stripe.com".into()]),
            },
            Ipv4Addr::new(1, 1, 1, 1),
        )
        .unwrap();

        assert!(policy.has_dns_filter());
        let blocked = super::DnsFilter::new(vec!["stripe.com".into()], "1.1.1.1".into());
        let query = {
            let mut packet = vec![
                0x12, 0x34, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ];
            for label in "attacker.com".split('.') {
                packet.push(label.len() as u8);
                packet.extend_from_slice(label.as_bytes());
            }
            packet.push(0x00);
            packet.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]);
            packet
        };
        let filtered = policy.filter_dns_query(&query).unwrap();
        let expected = blocked.handle_query(&query);
        assert_eq!(filtered, expected);
    }
}
