//! Bounded, allowlisted TCP/UDP relay for Moonlight's Sunshine protocol ports.
//!
//! This deliberately is not a generic port forwarder. `bind` exposes only the
//! canonical offsets from Sunshine's configured HTTPS/base port. Sunshine
//! documents TCP `-5`, `0`, and `+21`, and UDP `+9`, `+10`, and `+11`;
//! Moonlight's wake code additionally sends its magic packet to UDP `+13` and
//! `+21`. The Sunshine administration port at `+1` is intentionally absent.

use std::{
    collections::HashMap,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    ops::RangeInclusive,
    sync::Arc,
    time::Duration,
};

use tokio::{
    io::copy_bidirectional,
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{mpsc, watch, Semaphore},
    task::{JoinHandle, JoinSet},
    time::{sleep, timeout, Instant},
};

pub(crate) const TCP_PORT_OFFSETS: [i16; 3] = [-5, 0, 21];
pub(crate) const UDP_PORT_OFFSETS: [i16; 5] = [9, 10, 11, 13, 21];

const UDP_FLOW_QUEUE: usize = 64;
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(50);

/// Configuration supplied by the relay manager.
///
/// `bind_addr.port()` is Sunshine's configured HTTPS/base port. The manager
/// must supply one concrete client IP; there is no wildcard allowlist mode.
#[derive(Clone, Debug)]
pub(crate) struct MoonlightRelayConfig {
    pub(crate) bind_addr: SocketAddr,
    pub(crate) guest_ipv4: Ipv4Addr,
    pub(crate) guest_anchor: u16,
    pub(crate) allowed_client_ip: IpAddr,
    pub(crate) allowed_anchor_ports: RangeInclusive<u16>,
    pub(crate) max_tcp_connections: usize,
    pub(crate) max_udp_peers_per_port: usize,
    pub(crate) udp_idle_timeout: Duration,
    pub(crate) tcp_connect_timeout: Duration,
}

struct BoundTcpPort {
    listener: TcpListener,
    guest_addr: SocketAddr,
}

struct BoundUdpPort {
    socket: UdpSocket,
    guest_addr: SocketAddr,
}

/// All sockets have been acquired, but no relay tasks have started yet.
pub(crate) struct BoundMoonlightRelay {
    config: MoonlightRelayConfig,
    tcp: Vec<BoundTcpPort>,
    udp: Vec<BoundUdpPort>,
}

/// Running relay. Dropping it cancels and aborts the complete owned task tree;
/// callers that need proof that every flow socket is closed must call `stop`.
pub(crate) struct MoonlightRelay {
    cancel: watch::Sender<bool>,
    supervisor: Option<JoinHandle<()>>,
}

/// Atomically bind every canonical Moonlight/Sunshine relay socket.
///
/// If any bind fails, the already-bound sockets are dropped before the error
/// is returned and no serving task has been created.
pub(crate) async fn bind(config: MoonlightRelayConfig) -> io::Result<BoundMoonlightRelay> {
    bind_with_offsets(config, &TCP_PORT_OFFSETS, &UDP_PORT_OFFSETS).await
}

/// Bind all canonical sockets and start serving them.
pub(crate) async fn start(config: MoonlightRelayConfig) -> io::Result<MoonlightRelay> {
    Ok(bind(config).await?.start())
}

impl BoundMoonlightRelay {
    /// Start a single supervisor that owns every listener and descendant flow.
    pub(crate) fn start(self) -> MoonlightRelay {
        let (cancel, cancel_rx) = watch::channel(false);
        let supervisor = tokio::spawn(run_supervisor(self, cancel_rx));
        MoonlightRelay {
            cancel,
            supervisor: Some(supervisor),
        }
    }
}

impl MoonlightRelay {
    /// Cancel and await the entire task tree. On return, no listener or UDP
    /// per-client flow socket owned by this relay remains open.
    pub(crate) async fn stop(mut self) -> io::Result<()> {
        let _ = self.cancel.send(true);
        let Some(supervisor) = self.supervisor.take() else {
            return Ok(());
        };
        supervisor
            .await
            .map_err(|_| io::Error::other("Moonlight relay task failed"))
    }
}

impl Drop for MoonlightRelay {
    fn drop(&mut self) {
        let _ = self.cancel.send(true);
        // JoinSet drops recursively abort descendants if graceful cancellation
        // cannot run before this detached supervisor is polled again.
        if let Some(supervisor) = self.supervisor.take() {
            supervisor.abort();
        }
    }
}

async fn bind_with_offsets(
    config: MoonlightRelayConfig,
    tcp_offsets: &[i16],
    udp_offsets: &[i16],
) -> io::Result<BoundMoonlightRelay> {
    validate_config(&config)?;

    let anchor = config.bind_addr.port();
    let mut tcp = Vec::with_capacity(tcp_offsets.len());
    let mut udp = Vec::with_capacity(udp_offsets.len());

    for &offset in tcp_offsets {
        let public_port = offset_port(anchor, offset)?;
        let guest_port = offset_port(config.guest_anchor, offset)?;
        let addr = SocketAddr::new(config.bind_addr.ip(), public_port);
        let listener = TcpListener::bind(addr).await?;
        tcp.push(BoundTcpPort {
            listener,
            guest_addr: SocketAddr::new(IpAddr::V4(config.guest_ipv4), guest_port),
        });
    }

    for &offset in udp_offsets {
        let public_port = offset_port(anchor, offset)?;
        let guest_port = offset_port(config.guest_anchor, offset)?;
        let addr = SocketAddr::new(config.bind_addr.ip(), public_port);
        let socket = UdpSocket::bind(addr).await?;
        udp.push(BoundUdpPort {
            socket,
            guest_addr: SocketAddr::new(IpAddr::V4(config.guest_ipv4), guest_port),
        });
    }

    Ok(BoundMoonlightRelay { config, tcp, udp })
}

fn validate_config(config: &MoonlightRelayConfig) -> io::Result<()> {
    let anchor = config.bind_addr.port();
    if !config.allowed_anchor_ports.contains(&anchor) {
        return Err(invalid_input(
            "Moonlight anchor port is outside the allowed range",
        ));
    }
    if config.allowed_client_ip.is_unspecified() || config.allowed_client_ip.is_multicast() {
        return Err(invalid_input(
            "Moonlight relay requires one concrete client IP",
        ));
    }
    if config.guest_ipv4.is_unspecified()
        || config.guest_ipv4.is_multicast()
        || config.guest_ipv4 == Ipv4Addr::BROADCAST
    {
        return Err(invalid_input(
            "Moonlight relay requires one concrete guest IPv4 address",
        ));
    }
    if config.max_tcp_connections == 0 || config.max_udp_peers_per_port == 0 {
        return Err(invalid_input("Moonlight relay bounds must be nonzero"));
    }
    if config.udp_idle_timeout.is_zero() || config.tcp_connect_timeout.is_zero() {
        return Err(invalid_input("Moonlight relay timeouts must be nonzero"));
    }
    Ok(())
}

fn offset_port(anchor: u16, offset: i16) -> io::Result<u16> {
    let port = i32::from(anchor) + i32::from(offset);
    u16::try_from(port)
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| invalid_input("Moonlight port offset is outside the usable port range"))
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

async fn run_supervisor(bound: BoundMoonlightRelay, cancel: watch::Receiver<bool>) {
    let tcp_limit = Arc::new(Semaphore::new(bound.config.max_tcp_connections));
    let mut listeners = JoinSet::new();

    for port in bound.tcp {
        listeners.spawn(run_tcp_port(
            port,
            bound.config.allowed_client_ip,
            bound.config.tcp_connect_timeout,
            Arc::clone(&tcp_limit),
            cancel.clone(),
        ));
    }
    for port in bound.udp {
        listeners.spawn(run_udp_port(
            port,
            bound.config.allowed_client_ip,
            bound.config.max_udp_peers_per_port,
            bound.config.udp_idle_timeout,
            cancel.clone(),
        ));
    }

    let mut supervisor_cancel = cancel;
    if !*supervisor_cancel.borrow() {
        let _ = supervisor_cancel.changed().await;
    }
    while listeners.join_next().await.is_some() {}
}

async fn run_tcp_port(
    port: BoundTcpPort,
    allowed_client_ip: IpAddr,
    connect_timeout: Duration,
    limit: Arc<Semaphore>,
    mut cancel: watch::Receiver<bool>,
) {
    let mut connections = JoinSet::new();

    loop {
        tokio::select! {
            biased;
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() {
                    break;
                }
            }
            accepted = port.listener.accept() => {
                let (client, peer) = match accepted {
                    Ok(value) => value,
                    Err(_) => {
                        tokio::select! {
                            _ = sleep(ACCEPT_ERROR_BACKOFF) => {}
                            _ = cancel.changed() => { break; }
                        }
                        continue;
                    }
                };
                if peer.ip() != allowed_client_ip {
                    drop(client);
                    continue;
                }
                let Ok(permit) = Arc::clone(&limit).try_acquire_owned() else {
                    drop(client);
                    continue;
                };
                let guest_addr = port.guest_addr;
                let child_cancel = cancel.clone();
                connections.spawn(async move {
                    let _permit = permit;
                    relay_tcp(client, guest_addr, connect_timeout, child_cancel).await;
                });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }

    while connections.join_next().await.is_some() {}
}

async fn relay_tcp(
    mut client: TcpStream,
    guest_addr: SocketAddr,
    connect_timeout: Duration,
    mut cancel: watch::Receiver<bool>,
) {
    let guest = tokio::select! {
        biased;
        _ = cancel.changed() => { return; }
        result = timeout(connect_timeout, TcpStream::connect(guest_addr)) => {
            match result {
                Ok(Ok(stream)) => stream,
                _ => return,
            }
        }
    };
    let mut guest = guest;
    tokio::select! {
        biased;
        _ = cancel.changed() => {}
        _ = copy_bidirectional(&mut client, &mut guest) => {}
    }
}

struct UdpFlow {
    id: u64,
    tx: mpsc::Sender<Vec<u8>>,
}

async fn run_udp_port(
    port: BoundUdpPort,
    allowed_client_ip: IpAddr,
    max_peers: usize,
    idle_timeout: Duration,
    mut cancel: watch::Receiver<bool>,
) {
    let public_socket = Arc::new(port.socket);
    let (completed_tx, mut completed_rx) = mpsc::channel::<(SocketAddr, u64)>(max_peers);
    let mut flows: HashMap<SocketAddr, UdpFlow> = HashMap::new();
    let mut flow_tasks = JoinSet::new();
    let mut next_flow_id = 0_u64;
    let mut packet = vec![0_u8; 65_535];

    loop {
        tokio::select! {
            biased;
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() {
                    break;
                }
            }
            Some((peer, id)) = completed_rx.recv() => {
                if flows.get(&peer).is_some_and(|flow| flow.id == id) {
                    flows.remove(&peer);
                }
            }
            Some(_) = flow_tasks.join_next(), if !flow_tasks.is_empty() => {}
            received = public_socket.recv_from(&mut packet) => {
                let (length, peer) = match received {
                    Ok(value) => value,
                    Err(_) => {
                        tokio::select! {
                            _ = sleep(ACCEPT_ERROR_BACKOFF) => {}
                            _ = cancel.changed() => { break; }
                        }
                        continue;
                    }
                };
                // Unauthorized datagrams are discarded before allocating a
                // socket, queue, task, or reply, preventing reflection/amplification.
                if peer.ip() != allowed_client_ip {
                    continue;
                }

                if !flows.contains_key(&peer) {
                    if flows.len() >= max_peers {
                        continue;
                    }
                    let guest_socket = match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await {
                        Ok(socket) => socket,
                        Err(_) => continue,
                    };
                    if guest_socket.connect(port.guest_addr).await.is_err() {
                        continue;
                    }
                    next_flow_id = next_flow_id.wrapping_add(1);
                    let id = next_flow_id;
                    let (tx, rx) = mpsc::channel(UDP_FLOW_QUEUE);
                    flows.insert(peer, UdpFlow { id, tx });
                    flow_tasks.spawn(run_udp_flow(
                        guest_socket,
                        Arc::clone(&public_socket),
                        peer,
                        id,
                        rx,
                        completed_tx.clone(),
                        idle_timeout,
                        cancel.clone(),
                    ));
                }

                if let Some(flow) = flows.get(&peer) {
                    let _ = flow.tx.try_send(packet[..length].to_vec());
                }
            }
        }
    }

    drop(flows);
    while flow_tasks.join_next().await.is_some() {}
}

#[allow(clippy::too_many_arguments)]
async fn run_udp_flow(
    guest_socket: UdpSocket,
    public_socket: Arc<UdpSocket>,
    peer: SocketAddr,
    id: u64,
    mut client_packets: mpsc::Receiver<Vec<u8>>,
    completed: mpsc::Sender<(SocketAddr, u64)>,
    idle_timeout: Duration,
    mut cancel: watch::Receiver<bool>,
) {
    let idle = sleep(idle_timeout);
    tokio::pin!(idle);
    let mut reply = vec![0_u8; 65_535];

    loop {
        tokio::select! {
            biased;
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() {
                    break;
                }
            }
            () = &mut idle => { break; }
            packet = client_packets.recv() => {
                let Some(packet) = packet else { break; };
                if guest_socket.send(&packet).await.is_err() {
                    break;
                }
                idle.as_mut().reset(Instant::now() + idle_timeout);
            }
            received = guest_socket.recv(&mut reply) => {
                let length = match received {
                    Ok(length) => length,
                    Err(_) => break,
                };
                // Always return through the original public port to the exact
                // allowed client endpoint that created this flow.
                if public_socket.send_to(&reply[..length], peer).await.is_err() {
                    break;
                }
                idle.as_mut().reset(Instant::now() + idle_timeout);
            }
        }
    }

    let _ = completed.try_send((peer, id));
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn config(anchor: u16) -> MoonlightRelayConfig {
        MoonlightRelayConfig {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], anchor)),
            guest_ipv4: Ipv4Addr::new(127, 0, 0, 2),
            guest_anchor: anchor,
            allowed_client_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            allowed_anchor_ports: anchor..=anchor,
            max_tcp_connections: 4,
            max_udp_peers_per_port: 4,
            udp_idle_timeout: Duration::from_secs(2),
            tcp_connect_timeout: Duration::from_secs(1),
        }
    }

    async fn unused_tcp_port() -> u16 {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        listener.local_addr().unwrap().port()
    }

    #[tokio::test]
    async fn loopback_tcp_and_udp_baseline_preserves_udp_return_address() {
        let guest_ip = Ipv4Addr::new(127, 0, 0, 2);
        let guest_tcp = TcpListener::bind((guest_ip, 0)).await.unwrap();
        let guest_anchor = guest_tcp.local_addr().unwrap().port();
        let guest_udp = UdpSocket::bind((guest_ip, guest_anchor)).await.unwrap();
        let host_anchor = unused_tcp_port().await;
        let mut relay_config = config(host_anchor);
        relay_config.guest_anchor = guest_anchor;

        let relay = bind_with_offsets(relay_config, &[0], &[0])
            .await
            .unwrap()
            .start();

        let tcp_guest = tokio::spawn(async move {
            let (mut stream, _) = guest_tcp.accept().await.unwrap();
            let mut bytes = [0_u8; 4];
            stream.read_exact(&mut bytes).await.unwrap();
            stream.write_all(&bytes).await.unwrap();
        });
        let mut tcp_client = TcpStream::connect((Ipv4Addr::LOCALHOST, host_anchor))
            .await
            .unwrap();
        tcp_client.write_all(b"ping").await.unwrap();
        let mut echoed = [0_u8; 4];
        tcp_client.read_exact(&mut echoed).await.unwrap();
        assert_eq!(&echoed, b"ping");
        tcp_guest.await.unwrap();

        let udp_guest = tokio::spawn(async move {
            let mut bytes = [0_u8; 16];
            let (length, relay_flow) = guest_udp.recv_from(&mut bytes).await.unwrap();
            assert_eq!(&bytes[..length], b"media");
            guest_udp.send_to(b"reply", relay_flow).await.unwrap();
        });
        let udp_client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let client_addr = udp_client.local_addr().unwrap();
        udp_client
            .send_to(b"media", (Ipv4Addr::LOCALHOST, host_anchor))
            .await
            .unwrap();
        let mut bytes = [0_u8; 16];
        let (length, source) = timeout(Duration::from_secs(1), udp_client.recv_from(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&bytes[..length], b"reply");
        assert_eq!(source, SocketAddr::from((Ipv4Addr::LOCALHOST, host_anchor)));
        assert_eq!(udp_client.local_addr().unwrap(), client_addr);
        udp_guest.await.unwrap();

        relay.stop().await.unwrap();
    }

    #[tokio::test]
    async fn partial_bind_failure_releases_earlier_sockets() {
        let anchor = unused_tcp_port().await;
        let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, anchor + 1))
            .await
            .unwrap();

        let result = bind_with_offsets(config(anchor), &[0, 1], &[]).await;
        assert!(result.is_err());

        // The first socket was held only by the failed aggregate bind.
        TcpListener::bind((Ipv4Addr::LOCALHOST, anchor))
            .await
            .unwrap();
        drop(occupied);
    }

    #[tokio::test]
    async fn awaited_stop_closes_listener_and_active_udp_flow() {
        let guest_ip = Ipv4Addr::new(127, 0, 0, 2);
        let guest_tcp = TcpListener::bind((guest_ip, 0)).await.unwrap();
        let guest_anchor = guest_tcp.local_addr().unwrap().port();
        let guest_udp = UdpSocket::bind((guest_ip, guest_anchor)).await.unwrap();
        let host_anchor = unused_tcp_port().await;
        let mut relay_config = config(host_anchor);
        relay_config.guest_anchor = guest_anchor;
        let relay = bind_with_offsets(relay_config, &[0], &[0])
            .await
            .unwrap()
            .start();

        let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        client
            .send_to(b"open-flow", (Ipv4Addr::LOCALHOST, host_anchor))
            .await
            .unwrap();
        let mut packet = [0_u8; 32];
        timeout(Duration::from_secs(1), guest_udp.recv_from(&mut packet))
            .await
            .unwrap()
            .unwrap();

        relay.stop().await.unwrap();

        TcpListener::bind((Ipv4Addr::LOCALHOST, host_anchor))
            .await
            .unwrap();
        UdpSocket::bind((Ipv4Addr::LOCALHOST, host_anchor))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn canonical_ports_exclude_admin_offset() {
        assert_eq!(TCP_PORT_OFFSETS, [-5, 0, 21]);
        assert_eq!(UDP_PORT_OFFSETS, [9, 10, 11, 13, 21]);
        assert!(!TCP_PORT_OFFSETS.contains(&1));
        assert!(!UDP_PORT_OFFSETS.contains(&1));
    }
}
