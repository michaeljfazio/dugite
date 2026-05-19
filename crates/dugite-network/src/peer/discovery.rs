//! Peer discovery — DNS, ledger-based, and peer sharing.
//!
//! Provides three discovery mechanisms:
//! - **DNS**: SRV-first then A/AAAA fallback via hickory-resolver
//! - **Ledger**: SPO relay addresses from pool_params (when past useLedgerAfterSlot)
//! - **PeerSharing**: Addresses received from the PeerSharing protocol
//!
//! ## SRV resolution
//!
//! Cardano relays may publish DNS SRV records under `_cardano._tcp.<host>`.
//! The Haskell `cardano-node` tries SRV first and falls back to A/AAAA if no
//! records exist.  This module mirrors that behaviour:
//!
//! 1. Query `_cardano._tcp.<host>` for SRV records.
//! 2. Sort results by ascending priority; within a priority group apply a
//!    weighted shuffle (RFC 2782).
//! 3. For each SRV record: use the SRV port unless the caller supplied a
//!    non-zero override port (topology entry's port).
//! 4. Resolve each SRV target via A/AAAA to obtain concrete `(IP, port)` pairs.
//! 5. If no SRV records exist (NXDOMAIN / NOERROR with empty answer), fall back
//!    to a direct A/AAAA lookup on the original hostname using the caller's port.

use std::net::{IpAddr, SocketAddr};

/// A discovered peer address with its source.
#[derive(Debug, Clone)]
pub struct DiscoveredPeer {
    /// Socket address of the peer.
    pub addr: SocketAddr,
    /// How the peer was discovered.
    pub source: super::manager::PeerSource,
}

// ── Resolver trait (testable abstraction) ────────────────────────────────────

/// An SRV record returned by the DNS resolver.
#[derive(Debug, Clone)]
pub struct SrvRecord {
    /// Priority (lower = preferred).
    pub priority: u16,
    /// Weight for probabilistic selection within a priority group.
    pub weight: u16,
    /// Port advertised by the SRV record.
    pub port: u16,
    /// Target hostname to resolve for IPs.
    pub target: String,
    /// Resolved IP addresses for the target (may be empty if glue was absent
    /// and a follow-up A/AAAA lookup is needed).
    pub ips: Vec<IpAddr>,
}

/// Abstraction over DNS resolution, injectable for unit tests.
#[async_trait::async_trait]
pub trait DnsResolver: Send + Sync {
    /// Perform an SRV lookup on `_cardano._tcp.<host>`.
    ///
    /// Returns `Ok(vec![])` (not an error) when no SRV records exist so
    /// callers can distinguish "no records" from a hard DNS failure.
    async fn srv_lookup(&self, host: &str) -> Result<Vec<SrvRecord>, String>;

    /// Perform an A/AAAA lookup on `host`, returning all IP addresses.
    async fn a_aaaa_lookup(&self, host: &str) -> Vec<IpAddr>;
}

/// Production resolver backed by `hickory_resolver::TokioResolver`.
pub struct HickoryDnsResolver {
    inner: hickory_resolver::TokioResolver,
}

impl HickoryDnsResolver {
    /// Build a resolver from the system configuration.
    pub fn new() -> Result<Self, hickory_resolver::ResolveError> {
        let resolver = hickory_resolver::TokioResolver::builder_tokio()?.build();
        Ok(Self { inner: resolver })
    }
}

#[async_trait::async_trait]
impl DnsResolver for HickoryDnsResolver {
    async fn srv_lookup(&self, host: &str) -> Result<Vec<SrvRecord>, String> {
        let srv_name = format!("_cardano._tcp.{host}");
        match self.inner.srv_lookup(srv_name.as_str()).await {
            Ok(lookup) => {
                let mut records: Vec<SrvRecord> = Vec::new();
                for srv in lookup.iter() {
                    let target = srv.target().to_string();
                    // Strip the trailing dot that hickory appends to FQDN names.
                    let target = target.trim_end_matches('.').to_string();
                    // Collect any glue IPs bundled in the additional section.
                    let ips: Vec<IpAddr> = lookup.ip_iter().collect();
                    records.push(SrvRecord {
                        priority: srv.priority(),
                        weight: srv.weight(),
                        port: srv.port(),
                        target,
                        ips,
                    });
                }
                Ok(records)
            }
            Err(e) => {
                // NXDOMAIN or NOERROR/empty answer → treat as "no SRV records".
                if e.is_nx_domain() || e.is_no_records_found() {
                    Ok(vec![])
                } else {
                    Err(e.to_string())
                }
            }
        }
    }

    async fn a_aaaa_lookup(&self, host: &str) -> Vec<IpAddr> {
        match self.inner.lookup_ip(host).await {
            Ok(response) => response.iter().collect(),
            Err(e) => {
                tracing::debug!(error = %e, hostname = host, "A/AAAA lookup failed");
                vec![]
            }
        }
    }
}

// ── Weighted shuffle (RFC 2782) ───────────────────────────────────────────────

/// Apply RFC 2782 weighted shuffle in-place to a slice of SRV records that
/// all share the same priority.  Records with weight 0 are placed first (as
/// the spec recommends) then the rest are chosen proportionally.
fn weighted_shuffle(group: &mut Vec<SrvRecord>) {
    use rand::Rng;

    if group.len() <= 1 {
        return;
    }

    let mut rng = rand::rng();
    let mut result: Vec<SrvRecord> = Vec::with_capacity(group.len());
    // `pool` tracks which indices from `group` are still available.
    let mut pool: Vec<usize> = (0..group.len()).collect();

    while !pool.is_empty() {
        let total: u32 = pool.iter().map(|&i| u32::from(group[i].weight)).sum();

        let chosen_pool_idx = if total == 0 {
            // All remaining weights are 0 — pick uniformly at random.
            rng.random_range(0..pool.len())
        } else {
            let mut pick: u32 = rng.random_range(0..=total);
            let mut chosen = 0usize;
            for (pool_idx, &group_idx) in pool.iter().enumerate() {
                let w = u32::from(group[group_idx].weight);
                if pick <= w {
                    chosen = pool_idx;
                    break;
                }
                pick -= w;
                chosen = pool_idx; // last resort fallback
            }
            chosen
        };

        let group_idx = pool.remove(chosen_pool_idx);
        // We can't move out of a vec we're still indexing, so swap to end and pop.
        // We'll reconstruct from `pool` ordering; just record which indices were chosen.
        result.push(group[group_idx].clone());
    }

    *group = result;
}

// ── Core resolution logic ─────────────────────────────────────────────────────

/// Resolve a topology access point to concrete socket addresses.
///
/// Implements the Haskell `cardano-node` SRV-first resolution strategy:
///
/// 1. If `host` is already an IP address, return `(ip, port)` directly.
/// 2. Try `_cardano._tcp.<host>` SRV lookup.
/// 3. On success: sort by priority, weighted-shuffle within each priority,
///    resolve each SRV target via A/AAAA, use SRV port unless
///    `topology_port != 0`.
/// 4. On no SRV records: fall back to A/AAAA on the original `host` using
///    `topology_port`.
///
/// `topology_port == 0` is the convention that means "use whatever port the
/// SRV record says".  Any non-zero value overrides the SRV port.
pub async fn resolve_with_srv(
    resolver: &dyn DnsResolver,
    host: &str,
    topology_port: u16,
) -> Vec<SocketAddr> {
    // Fast path: literal IP address — no DNS needed.
    if let Ok(ip) = host.parse::<IpAddr>() {
        return vec![SocketAddr::new(ip, topology_port)];
    }

    // Try SRV first.
    match resolver.srv_lookup(host).await {
        Ok(records) if !records.is_empty() => {
            srv_records_to_addrs(resolver, records, topology_port).await
        }
        Ok(_empty) => {
            // No SRV records — fall back to direct A/AAAA.
            tracing::debug!(hostname = host, "no SRV records, falling back to A/AAAA");
            a_aaaa_to_addrs(resolver, host, topology_port).await
        }
        Err(e) => {
            // Hard DNS error — still attempt A/AAAA fallback.
            tracing::debug!(
                error = %e,
                hostname = host,
                "SRV lookup error, falling back to A/AAAA"
            );
            a_aaaa_to_addrs(resolver, host, topology_port).await
        }
    }
}

/// Convert a non-empty list of SRV records to socket addresses.
///
/// Sorts by priority (ascending), then applies weighted shuffle within each
/// priority group per RFC 2782.  For each record the IPs are resolved from
/// the glue section first, falling back to an explicit A/AAAA lookup on the
/// target hostname.
async fn srv_records_to_addrs(
    resolver: &dyn DnsResolver,
    mut records: Vec<SrvRecord>,
    topology_port: u16,
) -> Vec<SocketAddr> {
    // Sort ascending by priority so the lowest (= most preferred) comes first.
    records.sort_by_key(|r| r.priority);

    // Group by priority and apply weighted shuffle within each group.
    let mut priority_groups: Vec<Vec<SrvRecord>> = Vec::new();
    for record in records {
        if let Some(last) = priority_groups.last_mut() {
            if last[0].priority == record.priority {
                last.push(record);
                continue;
            }
        }
        priority_groups.push(vec![record]);
    }
    for group in &mut priority_groups {
        weighted_shuffle(group);
    }

    let mut addrs: Vec<SocketAddr> = Vec::new();
    for group in priority_groups {
        for srv in group {
            // Port: topology_port overrides if non-zero, otherwise use SRV port.
            let port = if topology_port != 0 {
                topology_port
            } else {
                srv.port
            };

            if !srv.ips.is_empty() {
                // Glue records present in the SRV response — use them directly.
                for ip in &srv.ips {
                    addrs.push(SocketAddr::new(*ip, port));
                }
            } else {
                // No glue — resolve the SRV target via A/AAAA.
                let resolved = resolver.a_aaaa_lookup(&srv.target).await;
                for ip in resolved {
                    addrs.push(SocketAddr::new(ip, port));
                }
            }
        }
    }

    addrs
}

/// Perform an A/AAAA lookup on `host` and map results to `SocketAddr`.
async fn a_aaaa_to_addrs(resolver: &dyn DnsResolver, host: &str, port: u16) -> Vec<SocketAddr> {
    resolver
        .a_aaaa_lookup(host)
        .await
        .into_iter()
        .map(|ip| SocketAddr::new(ip, port))
        .collect()
}

// ── Public convenience wrappers (keep existing callers working) ───────────────

/// Resolve a DNS hostname to socket addresses using the system resolver.
///
/// Uses hickory-resolver for async DNS resolution. Returns all A and AAAA
/// records for the hostname with the given port.
pub async fn resolve_dns(hostname: &str, port: u16) -> Vec<SocketAddr> {
    match HickoryDnsResolver::new() {
        Ok(resolver) => a_aaaa_to_addrs(&resolver, hostname, port).await,
        Err(e) => {
            tracing::warn!(error = %e, hostname, "failed to create DNS resolver");
            vec![]
        }
    }
}

/// Parse relay addresses from a topology configuration using SRV-first resolution.
///
/// For each `(host, port)` pair: tries `_cardano._tcp.<host>` SRV first,
/// falls back to A/AAAA. Direct IP addresses bypass DNS entirely.
pub async fn resolve_topology_relays(relays: &[(String, u16)]) -> Vec<SocketAddr> {
    match HickoryDnsResolver::new() {
        Ok(resolver) => {
            let mut addrs = Vec::new();
            for (host, port) in relays {
                let resolved = resolve_with_srv(&resolver, host, *port).await;
                addrs.extend(resolved);
            }
            addrs
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to create DNS resolver");
            vec![]
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    // ── Mock resolver ─────────────────────────────────────────────────────────

    /// Configurable mock for unit tests.
    struct MockResolver {
        /// SRV records returned for any host (Ok(vec![]) = no records, Err = hard fail).
        srv_result: Result<Vec<SrvRecord>, String>,
        /// IPs returned for any A/AAAA lookup.
        a_aaaa_ips: Vec<IpAddr>,
    }

    #[async_trait::async_trait]
    impl DnsResolver for MockResolver {
        async fn srv_lookup(&self, _host: &str) -> Result<Vec<SrvRecord>, String> {
            self.srv_result.clone()
        }

        async fn a_aaaa_lookup(&self, _host: &str) -> Vec<IpAddr> {
            self.a_aaaa_ips.clone()
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn ipv4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    fn ipv6_loopback() -> IpAddr {
        IpAddr::V6(Ipv6Addr::LOCALHOST)
    }

    fn make_srv(
        priority: u16,
        weight: u16,
        port: u16,
        target: &str,
        ips: Vec<IpAddr>,
    ) -> SrvRecord {
        SrvRecord {
            priority,
            weight,
            port,
            target: target.to_string(),
            ips,
        }
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    /// IP address literals bypass SRV entirely.
    #[tokio::test]
    async fn resolve_ip_literal_bypasses_srv() {
        let resolver = MockResolver {
            // These would never be called for an IP literal, but set to
            // something distinctive so we'd notice if they were.
            srv_result: Ok(vec![make_srv(1, 1, 9999, "should.not.appear", vec![])]),
            a_aaaa_ips: vec![ipv4(99, 99, 99, 99)],
        };
        let addrs = resolve_with_srv(&resolver, "1.2.3.4", 3001).await;
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0], "1.2.3.4:3001".parse().unwrap());
    }

    /// IPv6 literals also bypass SRV.
    #[tokio::test]
    async fn resolve_ipv6_literal_bypasses_srv() {
        let resolver = MockResolver {
            srv_result: Ok(vec![]),
            a_aaaa_ips: vec![],
        };
        let addrs = resolve_with_srv(&resolver, "::1", 3001).await;
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].port(), 3001);
        assert!(addrs[0].is_ipv6());
    }

    /// SRV returns 2 records; they are sorted by priority.
    /// Higher-priority (lower numeric value) record appears first.
    #[tokio::test]
    async fn srv_records_sorted_by_priority() {
        let ip_prio2 = ipv4(10, 0, 0, 2);
        let ip_prio1 = ipv4(10, 0, 0, 1);

        let resolver = MockResolver {
            srv_result: Ok(vec![
                // Deliberately out of priority order.
                make_srv(20, 10, 3002, "relay-b.example.com", vec![ip_prio2]),
                make_srv(10, 10, 3001, "relay-a.example.com", vec![ip_prio1]),
            ]),
            a_aaaa_ips: vec![],
        };

        let addrs = resolve_with_srv(&resolver, "example.com", 0).await;
        // Expect two addresses; priority 10 (relay-a) before priority 20 (relay-b).
        assert_eq!(addrs.len(), 2, "expected 2 resolved addresses");
        assert_eq!(
            addrs[0].ip(),
            ip_prio1,
            "priority-10 record should come first"
        );
        assert_eq!(
            addrs[1].ip(),
            ip_prio2,
            "priority-20 record should come second"
        );
    }

    /// SRV port is used when topology_port == 0.
    #[tokio::test]
    async fn srv_port_used_when_topology_port_is_zero() {
        let resolver = MockResolver {
            srv_result: Ok(vec![make_srv(
                1,
                1,
                4444,
                "relay.example.com",
                vec![ipv4(1, 2, 3, 4)],
            )]),
            a_aaaa_ips: vec![],
        };
        let addrs = resolve_with_srv(&resolver, "example.com", 0).await;
        assert_eq!(addrs.len(), 1);
        assert_eq!(
            addrs[0].port(),
            4444,
            "SRV port should be used when topology_port == 0"
        );
    }

    /// Non-zero topology_port overrides the SRV record's port.
    #[tokio::test]
    async fn topology_port_overrides_srv_port() {
        let resolver = MockResolver {
            srv_result: Ok(vec![make_srv(
                1,
                1,
                4444,
                "relay.example.com",
                vec![ipv4(1, 2, 3, 4)],
            )]),
            a_aaaa_ips: vec![],
        };
        // topology_port = 3001 should override SRV port 4444.
        let addrs = resolve_with_srv(&resolver, "example.com", 3001).await;
        assert_eq!(addrs.len(), 1);
        assert_eq!(
            addrs[0].port(),
            3001,
            "topology port should override SRV port"
        );
    }

    /// No SRV records → fall back to A/AAAA with topology_port.
    #[tokio::test]
    async fn no_srv_falls_back_to_a_aaaa() {
        let resolver = MockResolver {
            srv_result: Ok(vec![]), // NXDOMAIN / no records
            a_aaaa_ips: vec![ipv4(5, 6, 7, 8)],
        };
        let addrs = resolve_with_srv(&resolver, "relay.example.com", 3001).await;
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].ip(), ipv4(5, 6, 7, 8));
        assert_eq!(addrs[0].port(), 3001);
    }

    /// Hard SRV DNS error → still fall back to A/AAAA.
    #[tokio::test]
    async fn srv_hard_error_falls_back_to_a_aaaa() {
        let resolver = MockResolver {
            srv_result: Err("SERVFAIL".to_string()),
            a_aaaa_ips: vec![ipv4(9, 10, 11, 12)],
        };
        let addrs = resolve_with_srv(&resolver, "relay.example.com", 3001).await;
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].ip(), ipv4(9, 10, 11, 12));
    }

    /// SRV record without glue IPs triggers a follow-up A/AAAA lookup on the target.
    #[tokio::test]
    async fn srv_target_resolved_when_no_glue() {
        let resolver = MockResolver {
            srv_result: Ok(vec![make_srv(
                1,
                1,
                3001,
                "target.example.com",
                vec![], // no glue
            )]),
            a_aaaa_ips: vec![ipv4(20, 21, 22, 23)], // returned by A/AAAA lookup
        };
        let addrs = resolve_with_srv(&resolver, "example.com", 0).await;
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].ip(), ipv4(20, 21, 22, 23));
        assert_eq!(addrs[0].port(), 3001);
    }

    /// Multiple IPs per SRV record — all returned.
    #[tokio::test]
    async fn srv_multiple_ips_all_returned() {
        let resolver = MockResolver {
            srv_result: Ok(vec![make_srv(
                1,
                1,
                3001,
                "relay.example.com",
                vec![ipv4(1, 1, 1, 1), ipv4(2, 2, 2, 2), ipv6_loopback()],
            )]),
            a_aaaa_ips: vec![],
        };
        let addrs = resolve_with_srv(&resolver, "example.com", 0).await;
        assert_eq!(addrs.len(), 3);
    }

    /// A/AAAA returns multiple IPs — all returned.
    #[tokio::test]
    async fn a_aaaa_multiple_ips_all_returned() {
        let resolver = MockResolver {
            srv_result: Ok(vec![]), // no SRV
            a_aaaa_ips: vec![ipv4(1, 1, 1, 1), ipv4(2, 2, 2, 2)],
        };
        let addrs = resolve_with_srv(&resolver, "relay.example.com", 3001).await;
        assert_eq!(addrs.len(), 2);
        for addr in &addrs {
            assert_eq!(addr.port(), 3001);
        }
    }

    /// Existing resolve_topology_relays direct-IP test (regression).
    #[tokio::test]
    async fn resolve_ip_address_directly() {
        let addrs = resolve_topology_relays(&[("127.0.0.1".to_string(), 3001)]).await;
        assert_eq!(addrs.len(), 1);
        assert_eq!(
            addrs[0],
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 3001)
        );
    }

    /// Existing resolve_topology_relays IPv6 test (regression).
    #[tokio::test]
    async fn resolve_ipv6_address_directly() {
        let addrs = resolve_topology_relays(&[("::1".to_string(), 3001)]).await;
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].port(), 3001);
    }
}
