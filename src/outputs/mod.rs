//! Playing on network devices: Sonos speakers (`sonos`) and Google Cast devices such as
//! Android TVs, Chromecasts and Nest speakers (`cast`). A device fetches each song from a
//! small media server in the player (`media`, with `wav` for files it cannot play) and is
//! driven through a background `link`. The local playback goes on muted, driving the
//! visualizers and the play order, and is kept in step with the device (`sync`).

pub mod card;
pub mod cast;
pub mod http;
pub mod link;
pub mod media;
pub mod pixels;
pub mod sonos;
pub mod sync;
pub mod video;
pub mod wav;

use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

use socket2::{Domain, Protocol, Socket, Type};

pub use link::{Command, Event, Link, SongInfo, Status, Timing, TransportState};

/// A device the player can play on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Device {
    /// A Sonos speaker or group.
    Sonos(sonos::Zone),
    /// A Google Cast device: an Android TV, a Chromecast, a Nest speaker.
    Cast(cast::CastDevice),
}

impl Device {
    /// "Living Room", "Living Room + Kitchen" (a Sonos group), "Televisio".
    pub fn name(&self) -> String {
        match self {
            Device::Sonos(zone) => zone.name(),
            Device::Cast(device) => device.name.clone(),
        }
    }

    /// Stays the same while the device is on the network.
    pub fn id(&self) -> &str {
        match self {
            Device::Sonos(zone) => &zone.uuid,
            Device::Cast(device) => &device.id,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Device::Sonos(_) => "Sonos",
            Device::Cast(_) => "Google Cast",
        }
    }

    /// Room names: a Sonos group's rooms; a Cast device is one.
    pub fn rooms(&self) -> Vec<String> {
        match self {
            Device::Sonos(zone) => zone.rooms.clone(),
            Device::Cast(device) => vec![device.name.clone()],
        }
    }

    /// The address the player talks to.
    pub fn addr(&self) -> SocketAddr {
        match self {
            Device::Sonos(zone) => zone.coordinator,
            Device::Cast(device) => device.addr,
        }
    }

    /// Starts a link to the device.
    pub fn connect(&self, timing: Timing) -> Link {
        match self {
            Device::Sonos(zone) => sonos::start(sonos::Speaker::new(zone.coordinator), timing),
            Device::Cast(device) => cast::start(device.clone(), timing),
        }
    }
}

/// Finds the devices to play on: Sonos speakers at `sonos_hosts`, and with `network`
/// everything that answers a search for `wait` (Sonos by SSDP, Google Cast by mDNS). An
/// error means devices answered but nothing could be used.
pub fn discover(
    sonos_hosts: &[String],
    network: bool,
    wait: Duration,
) -> Result<Vec<Device>, String> {
    let casts = network.then(|| std::thread::spawn(move || cast::discovery::discover(wait)));
    let mut devices = Vec::new();
    let mut error = None;
    match sonos::discovery::discover(sonos_hosts, network, wait) {
        Ok(zones) => devices.extend(zones.into_iter().map(Device::Sonos)),
        Err(e) => error = Some(e),
    }
    if let Some(found) = casts.and_then(|search| search.join().ok()) {
        devices.extend(found.into_iter().map(Device::Cast));
    }
    if devices.is_empty()
        && let Some(e) = error
    {
        return Err(e);
    }
    devices.sort_by(|a, b| crate::library::natural_cmp(&a.name(), &b.name()));
    Ok(devices)
}

/// A search goes out twice, in case the first is lost (UDP on Wi-Fi).
const RESEND_AFTER: Duration = Duration::from_millis(400);

fn search_socket(interface: Option<Ipv4Addr>, ttl: u32) -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    let ip = interface.unwrap_or(Ipv4Addr::UNSPECIFIED);
    socket.bind(&SocketAddrV4::new(ip, 0).into())?;
    if let Some(ip) = interface {
        // Without this the search only leaves through the default adapter, which on a PC
        // with virtual adapters (WSL, Hyper-V, VPNs) is often not the home network.
        socket.set_multicast_if_v4(&ip)?;
    }
    socket.set_multicast_ttl_v4(ttl)?;
    let socket: UdpSocket = socket.into();
    socket.set_read_timeout(Some(Duration::from_millis(20)))?;
    Ok(socket)
}

/// IPv4 addresses of this computer's network adapters (not loopback).
fn adapter_addresses() -> Vec<Ipv4Addr> {
    let mut ips: Vec<Ipv4Addr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(a) if !a.ip.is_loopback() => Some(a.ip),
            _ => None,
        })
        .collect();
    ips.sort();
    ips.dedup();
    ips
}

/// Sends `query` to the multicast `group` from every network adapter, with multicast `ttl`,
/// and hands each answer (sent back to the asking socket) to `answer` for `wait`.
pub(crate) fn multicast_search(
    group: SocketAddrV4,
    query: &[u8],
    ttl: u32,
    wait: Duration,
    mut answer: impl FnMut(&[u8], SocketAddr),
) {
    let mut sockets: Vec<UdpSocket> = adapter_addresses()
        .into_iter()
        .filter_map(|ip| match search_socket(Some(ip), ttl) {
            Ok(s) => Some(s),
            Err(e) => {
                log::debug!("search on {ip}: {e}");
                None
            }
        })
        .collect();
    if sockets.is_empty() {
        sockets.extend(search_socket(None, ttl).ok());
    }
    let send = |sockets: &[UdpSocket]| {
        for s in sockets {
            if let Err(e) = s.send_to(query, group) {
                log::debug!("search from {:?}: {e}", s.local_addr());
            }
        }
    };
    send(&sockets);
    let started = Instant::now();
    let mut resent = false;
    let mut buf = [0u8; 9000];
    while started.elapsed() < wait {
        if !resent && started.elapsed() >= RESEND_AFTER {
            send(&sockets);
            resent = true;
        }
        for s in &sockets {
            while let Ok((n, from)) = s.recv_from(&mut buf) {
                answer(&buf[..n], from);
            }
        }
    }
}
