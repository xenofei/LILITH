//! A process's sockets from the IP Helper tables (ARCH-006 checks; the engine loopback check
//! of ARCH-068).
//!
//! Read with `GetExtendedTcpTable` and `GetExtendedUdpTable` for IPv4 and IPv6, never by
//! parsing `netstat`, whose output is localised.

use std::ffi::c_void;
use std::io;
use std::mem::offset_of;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
use windows::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, GetExtendedUdpTable, MIB_TCP_STATE_LISTEN, MIB_TCP6ROW_OWNER_PID,
    MIB_TCP6TABLE_OWNER_PID, MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID, MIB_UDP6ROW_OWNER_PID,
    MIB_UDP6TABLE_OWNER_PID, MIB_UDPROW_OWNER_PID, MIB_UDPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_ALL,
    UDP_TABLE_OWNER_PID,
};
use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6};

/// One socket a process owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SocketKind {
    /// A TCP socket in the listening state.
    TcpListen,
    /// A TCP socket in any other state.
    TcpOther,
    /// A bound UDP socket.
    Udp,
}

/// Every TCP and UDP socket (IPv4 and IPv6) owned by `pid`.
pub fn sockets_of(pid: u32) -> io::Result<Vec<SocketKind>> {
    let listen = MIB_TCP_STATE_LISTEN.0 as u32;
    let tcp_kind = |state: u32| {
        if state == listen {
            SocketKind::TcpListen
        } else {
            SocketKind::TcpOther
        }
    };
    let mut found = Vec::new();

    let tcp4 = fetch(|buf, size| {
        // SAFETY: `buf` is a writable buffer of `*size` bytes, as the call requires.
        unsafe {
            GetExtendedTcpTable(
                buf,
                size,
                false,
                u32::from(AF_INET.0),
                TCP_TABLE_OWNER_PID_ALL,
                0,
            )
        }
    })?;
    // SAFETY: the buffer holds a MIB_TCPTABLE_OWNER_PID written by the call above.
    let rows = unsafe {
        table_rows::<MIB_TCPROW_OWNER_PID>(&tcp4, offset_of!(MIB_TCPTABLE_OWNER_PID, table))
    }?;
    found.extend(
        rows.iter()
            .filter(|r| r.dwOwningPid == pid)
            .map(|r| tcp_kind(r.dwState)),
    );

    let tcp6 = fetch(|buf, size| {
        // SAFETY: as above.
        unsafe {
            GetExtendedTcpTable(
                buf,
                size,
                false,
                u32::from(AF_INET6.0),
                TCP_TABLE_OWNER_PID_ALL,
                0,
            )
        }
    })?;
    // SAFETY: the buffer holds a MIB_TCP6TABLE_OWNER_PID written by the call above.
    let rows = unsafe {
        table_rows::<MIB_TCP6ROW_OWNER_PID>(&tcp6, offset_of!(MIB_TCP6TABLE_OWNER_PID, table))
    }?;
    found.extend(
        rows.iter()
            .filter(|r| r.dwOwningPid == pid)
            .map(|r| tcp_kind(r.dwState)),
    );

    let udp4 = fetch(|buf, size| {
        // SAFETY: as above.
        unsafe {
            GetExtendedUdpTable(
                buf,
                size,
                false,
                u32::from(AF_INET.0),
                UDP_TABLE_OWNER_PID,
                0,
            )
        }
    })?;
    // SAFETY: the buffer holds a MIB_UDPTABLE_OWNER_PID written by the call above.
    let rows = unsafe {
        table_rows::<MIB_UDPROW_OWNER_PID>(&udp4, offset_of!(MIB_UDPTABLE_OWNER_PID, table))
    }?;
    found.extend(
        rows.iter()
            .filter(|r| r.dwOwningPid == pid)
            .map(|_| SocketKind::Udp),
    );

    let udp6 = fetch(|buf, size| {
        // SAFETY: as above.
        unsafe {
            GetExtendedUdpTable(
                buf,
                size,
                false,
                u32::from(AF_INET6.0),
                UDP_TABLE_OWNER_PID,
                0,
            )
        }
    })?;
    // SAFETY: the buffer holds a MIB_UDP6TABLE_OWNER_PID written by the call above.
    let rows = unsafe {
        table_rows::<MIB_UDP6ROW_OWNER_PID>(&udp6, offset_of!(MIB_UDP6TABLE_OWNER_PID, table))
    }?;
    found.extend(
        rows.iter()
            .filter(|r| r.dwOwningPid == pid)
            .map(|_| SocketKind::Udp),
    );

    Ok(found)
}

/// Where the engine loopback check reads a process's listening sockets (ARCH-068): the
/// system's tables ([`OsSockets`]) in production; tests may rewrite rows so a check sees a
/// wildcard listener without one ever being bound (`LIL_TEST_FAKE_SOCKETS` in the core).
pub trait SocketTable: Send + Sync {
    /// The local address of every listening TCP socket (IPv4 and IPv6) that `pid` owns.
    fn listening(&self, pid: u32) -> io::Result<Vec<SocketAddr>>;
}

/// The system's TCP tables ([`listening_sockets`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct OsSockets;

impl SocketTable for OsSockets {
    fn listening(&self, pid: u32) -> io::Result<Vec<SocketAddr>> {
        listening_sockets(pid)
    }
}

/// The local address of every listening TCP socket (IPv4 and IPv6) that `pid` owns. A wildcard
/// listener shows as `0.0.0.0` or `[::]`.
pub fn listening_sockets(pid: u32) -> io::Result<Vec<SocketAddr>> {
    Ok(tcp_listeners()?
        .into_iter()
        .filter(|&(_, owner)| owner == pid)
        .map(|(addr, _)| addr)
        .collect())
}

/// The process with a TCP socket listening on `port`, on any local address; `None` when there
/// is none. When several listen on it, the first in the IPv4 table, then the IPv6 table.
pub fn pid_listening_on(port: u16) -> io::Result<Option<u32>> {
    Ok(tcp_listeners()?
        .into_iter()
        .find(|(addr, _)| addr.port() == port)
        .map(|(_, owner)| owner))
}

/// The port of a table row: the low 16 bits, in network byte order.
fn row_port(raw: u32) -> u16 {
    u16::from_be(raw as u16)
}

/// An IPv4 row's local address and owner, when the row is a listening socket.
fn listener_v4(r: &MIB_TCPROW_OWNER_PID) -> Option<(SocketAddr, u32)> {
    (r.dwState == MIB_TCP_STATE_LISTEN.0 as u32).then(|| {
        // The address is in network byte order: its bytes in memory are the octets.
        let ip = Ipv4Addr::from(r.dwLocalAddr.to_ne_bytes());
        let addr = SocketAddr::V4(SocketAddrV4::new(ip, row_port(r.dwLocalPort)));
        (addr, r.dwOwningPid)
    })
}

/// An IPv6 row's local address and owner, when the row is a listening socket.
fn listener_v6(r: &MIB_TCP6ROW_OWNER_PID) -> Option<(SocketAddr, u32)> {
    (r.dwState == MIB_TCP_STATE_LISTEN.0 as u32).then(|| {
        let ip = Ipv6Addr::from(r.ucLocalAddr);
        let addr = SocketAddr::V6(SocketAddrV6::new(
            ip,
            row_port(r.dwLocalPort),
            0,
            r.dwLocalScopeId,
        ));
        (addr, r.dwOwningPid)
    })
}

/// Every listening TCP socket: its local address and its owner's pid.
fn tcp_listeners() -> io::Result<Vec<(SocketAddr, u32)>> {
    let mut found = Vec::new();

    let tcp4 = fetch(|buf, size| {
        // SAFETY: `buf` is a writable buffer of `*size` bytes, as the call requires.
        unsafe {
            GetExtendedTcpTable(
                buf,
                size,
                false,
                u32::from(AF_INET.0),
                TCP_TABLE_OWNER_PID_ALL,
                0,
            )
        }
    })?;
    // SAFETY: the buffer holds a MIB_TCPTABLE_OWNER_PID written by the call above.
    let rows = unsafe {
        table_rows::<MIB_TCPROW_OWNER_PID>(&tcp4, offset_of!(MIB_TCPTABLE_OWNER_PID, table))
    }?;
    found.extend(rows.iter().filter_map(listener_v4));

    let tcp6 = fetch(|buf, size| {
        // SAFETY: as above.
        unsafe {
            GetExtendedTcpTable(
                buf,
                size,
                false,
                u32::from(AF_INET6.0),
                TCP_TABLE_OWNER_PID_ALL,
                0,
            )
        }
    })?;
    // SAFETY: the buffer holds a MIB_TCP6TABLE_OWNER_PID written by the call above.
    let rows = unsafe {
        table_rows::<MIB_TCP6ROW_OWNER_PID>(&tcp6, offset_of!(MIB_TCP6TABLE_OWNER_PID, table))
    }?;
    found.extend(rows.iter().filter_map(listener_v6));

    Ok(found)
}

/// Calls an IP Helper table function until the buffer is large enough. The buffer is `u64`s so
/// the table is suitably aligned.
fn fetch(call: impl Fn(Option<*mut c_void>, *mut u32) -> u32) -> io::Result<Vec<u64>> {
    let mut words = 1usize;
    for _ in 0..8 {
        let mut buf = vec![0u64; words];
        let mut size = (buf.len() * 8) as u32;
        match call(Some(buf.as_mut_ptr().cast()), &mut size) {
            rc if rc == NO_ERROR.0 => return Ok(buf),
            // `size` is now the size needed; leave headroom for sockets opened meanwhile.
            rc if rc == ERROR_INSUFFICIENT_BUFFER.0 => words = (size as usize).div_ceil(8) + 64,
            rc => return Err(io::Error::from_raw_os_error(rc as i32)),
        }
    }
    Err(io::Error::other("the socket table kept growing"))
}

/// The rows of a table that starts with a `u32` row count followed, at `offset`, by the rows.
///
/// # Safety
/// `buf` must hold a table of that shape whose rows are `T`.
unsafe fn table_rows<T: Copy>(buf: &[u64], offset: usize) -> io::Result<&[T]> {
    let base = buf.as_ptr().cast::<u8>();
    // SAFETY: the buffer is at least 8 bytes and starts with the row count.
    let count = unsafe { base.cast::<u32>().read() } as usize;
    let bytes = buf.len() * 8;
    if count
        .checked_mul(size_of::<T>())
        .and_then(|n| n.checked_add(offset))
        .is_none_or(|end| end > bytes)
    {
        return Err(io::Error::other(
            "the socket table is larger than its buffer",
        ));
    }
    // SAFETY: the rows lie inside `buf` (checked above) and are aligned, since `offset` is the
    // field offset in a struct whose alignment `u64` storage satisfies.
    Ok(unsafe { std::slice::from_raw_parts(base.add(offset).cast::<T>(), count) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::NetworkManagement::IpHelper::MIB_TCP_STATE_ESTAB;

    /// A port as the tables hold it: network byte order in the low 16 bits.
    fn raw_port(port: u16) -> u32 {
        u32::from(port.to_be())
    }

    fn v4(octets: [u8; 4], port: u16, state: u32, pid: u32) -> MIB_TCPROW_OWNER_PID {
        MIB_TCPROW_OWNER_PID {
            dwState: state,
            dwLocalAddr: u32::from_ne_bytes(octets),
            dwLocalPort: raw_port(port),
            dwRemoteAddr: 0,
            dwRemotePort: 0,
            dwOwningPid: pid,
        }
    }

    fn v6(addr: Ipv6Addr, port: u16, state: u32, pid: u32) -> MIB_TCP6ROW_OWNER_PID {
        MIB_TCP6ROW_OWNER_PID {
            ucLocalAddr: addr.octets(),
            dwLocalScopeId: 0,
            dwLocalPort: raw_port(port),
            ucRemoteAddr: [0; 16],
            dwRemoteScopeId: 0,
            dwRemotePort: 0,
            dwState: state,
            dwOwningPid: pid,
        }
    }

    /// Table rows become the addresses the loopback check reads: wildcard listeners as
    /// `0.0.0.0` and `[::]`, loopback ones as `127.0.0.1` and `[::1]`, and a connected socket not
    /// at all. Synthetic rows, so no test binds a wildcard socket (P3-R11).
    #[test] // spec: ARCH-068
    fn table_rows_convert_to_listening_addresses() {
        let listen = MIB_TCP_STATE_LISTEN.0 as u32;
        let established = MIB_TCP_STATE_ESTAB.0 as u32;
        let addr = |s: &str| s.parse::<SocketAddr>().unwrap();
        assert_eq!(
            listener_v4(&v4([0, 0, 0, 0], 8188, listen, 7)),
            Some((addr("0.0.0.0:8188"), 7))
        );
        assert_eq!(
            listener_v4(&v4([127, 0, 0, 1], 50_001, listen, 8)),
            Some((addr("127.0.0.1:50001"), 8))
        );
        assert_eq!(listener_v4(&v4([127, 0, 0, 1], 1, established, 8)), None);
        assert_eq!(
            listener_v6(&v6(Ipv6Addr::UNSPECIFIED, 8188, listen, 7)),
            Some((addr("[::]:8188"), 7))
        );
        assert_eq!(
            listener_v6(&v6(Ipv6Addr::LOCALHOST, 443, listen, 9)),
            Some((addr("[::1]:443"), 9))
        );
        assert_eq!(
            listener_v6(&v6(Ipv6Addr::LOCALHOST, 1, established, 9)),
            None
        );
    }
}
