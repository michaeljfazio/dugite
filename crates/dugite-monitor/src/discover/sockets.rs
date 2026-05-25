//! LISTEN-socket enumeration via `netstat2`. Maps each dugite-node PID
//! to the ports it has bound for listening.

use std::collections::{HashMap, HashSet};

use netstat2::{AddressFamilyFlags, ProtocolFlags, ProtocolSocketInfo, SocketInfo, TcpState};

/// For each PID in `pids`, return the list of TCP ports the process has
/// in the LISTEN state. PIDs with no listening ports are present in the
/// returned map with an empty Vec. Empty map on any netstat2 error.
pub(super) fn listen_ports_for_pids(pids: &HashSet<u32>) -> HashMap<u32, Vec<u16>> {
    let sockets = match netstat2::get_sockets_info(
        AddressFamilyFlags::IPV4 | AddressFamilyFlags::IPV6,
        ProtocolFlags::TCP,
    ) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "netstat2 get_sockets_info failed");
            return HashMap::new();
        }
    };
    filter_listening_for_pids(&sockets, pids)
}

/// Pure filter half of `listen_ports_for_pids`, exposed for unit tests.
fn filter_listening_for_pids(
    sockets: &[SocketInfo],
    pids: &HashSet<u32>,
) -> HashMap<u32, Vec<u16>> {
    let mut out: HashMap<u32, Vec<u16>> = HashMap::new();
    for pid in pids {
        out.entry(*pid).or_default();
    }
    for info in sockets {
        let tcp = match &info.protocol_socket_info {
            ProtocolSocketInfo::Tcp(t) => t,
            _ => continue,
        };
        if tcp.state != TcpState::Listen {
            continue;
        }
        for pid in &info.associated_pids {
            if pids.contains(pid) {
                out.entry(*pid).or_default().push(tcp.local_port);
            }
        }
    }
    for ports in out.values_mut() {
        ports.sort_unstable();
        ports.dedup();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use netstat2::TcpSocketInfo;
    use std::net::Ipv4Addr;

    fn tcp(pid: u32, port: u16, state: TcpState) -> SocketInfo {
        SocketInfo {
            protocol_socket_info: ProtocolSocketInfo::Tcp(TcpSocketInfo {
                local_addr: Ipv4Addr::LOCALHOST.into(),
                local_port: port,
                remote_addr: Ipv4Addr::UNSPECIFIED.into(),
                remote_port: 0,
                state,
            }),
            associated_pids: vec![pid],
            #[cfg(any(target_os = "linux", target_os = "android"))]
            inode: 0,
            #[cfg(any(target_os = "linux", target_os = "android"))]
            uid: 0,
        }
    }

    #[test]
    fn filter_picks_only_listening() {
        let pids: HashSet<u32> = [1234].into_iter().collect();
        let sockets = vec![
            tcp(1234, 12798, TcpState::Listen),
            tcp(1234, 3001, TcpState::Listen),
            tcp(1234, 50000, TcpState::Established),
        ];
        let out = filter_listening_for_pids(&sockets, &pids);
        assert_eq!(out.get(&1234), Some(&vec![3001, 12798]));
    }

    #[test]
    fn filter_excludes_unrelated_pids() {
        let pids: HashSet<u32> = [1234].into_iter().collect();
        let sockets = vec![
            tcp(9999, 12798, TcpState::Listen),
            tcp(1234, 12798, TcpState::Listen),
        ];
        let out = filter_listening_for_pids(&sockets, &pids);
        assert_eq!(out.get(&1234), Some(&vec![12798]));
        assert_eq!(out.get(&9999), None);
    }

    #[test]
    fn filter_returns_pid_with_empty_vec_when_no_sockets() {
        let pids: HashSet<u32> = [1234].into_iter().collect();
        let sockets = vec![];
        let out = filter_listening_for_pids(&sockets, &pids);
        assert_eq!(out.get(&1234), Some(&vec![]));
    }

    #[test]
    fn filter_dedups_duplicate_ports() {
        let pids: HashSet<u32> = [1234].into_iter().collect();
        let sockets = vec![
            tcp(1234, 12798, TcpState::Listen),
            tcp(1234, 12798, TcpState::Listen),
        ];
        let out = filter_listening_for_pids(&sockets, &pids);
        assert_eq!(out.get(&1234), Some(&vec![12798]));
    }
}
