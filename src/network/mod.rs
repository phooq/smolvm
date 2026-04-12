//! Network configuration and backend selection.

pub mod addressing;
/// Backend selection and serialization helpers.
pub mod backend;
/// Launch-time backend planning.
pub mod launch;
pub mod policy;
pub mod virtio;

pub use backend::NetworkBackend;
pub use launch::{plan_launch_network, EffectiveNetworkBackend, LaunchNetworkPlan};
pub use policy::{get_dns_server, LaunchEgressPolicy, ResolvedEgressPolicy};
