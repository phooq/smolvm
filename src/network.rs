//! Network configuration.
//!
//! This module provides network policy configuration for VMs.
//!
//! Phase 0: Types and DNS configuration.
//! Phase 1: Full NAT egress support via libkrun.

use crate::vm::config::NetworkPolicy;
use std::net::IpAddr;

/// Default DNS server (Cloudflare).
pub const DEFAULT_DNS: &str = "1.1.1.1";
/// Google's public DNS server.
pub const GOOGLE_DNS: &str = "8.8.8.8";

/// Get the DNS server for a network policy.
pub fn get_dns_server(policy: &NetworkPolicy) -> Option<IpAddr> {
    match policy {
        NetworkPolicy::None => None,
        NetworkPolicy::Egress { dns } => {
            Some(dns.unwrap_or_else(|| DEFAULT_DNS.parse().unwrap()))
        }
    }
}

/// Check if a network policy allows egress.
pub fn allows_egress(policy: &NetworkPolicy) -> bool {
    matches!(policy, NetworkPolicy::Egress { .. })
}

/// Create an egress policy with the default DNS.
pub fn egress_default() -> NetworkPolicy {
    NetworkPolicy::Egress { dns: None }
}

/// Create an egress policy with a custom DNS server.
pub fn egress_with_dns(dns: IpAddr) -> NetworkPolicy {
    NetworkPolicy::Egress { dns: Some(dns) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_dns_server_none() {
        assert!(get_dns_server(&NetworkPolicy::None).is_none());
    }

    #[test]
    fn test_get_dns_server_egress_default() {
        let dns = get_dns_server(&egress_default()).unwrap();
        assert_eq!(dns.to_string(), DEFAULT_DNS);
    }

    #[test]
    fn test_get_dns_server_egress_custom() {
        let custom: IpAddr = GOOGLE_DNS.parse().unwrap();
        let policy = egress_with_dns(custom);
        let dns = get_dns_server(&policy).unwrap();
        assert_eq!(dns.to_string(), GOOGLE_DNS);
    }

    #[test]
    fn test_allows_egress() {
        assert!(!allows_egress(&NetworkPolicy::None));
        assert!(allows_egress(&egress_default()));
    }
}
