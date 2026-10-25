//! Finding Sonos speakers: an SSDP search on every network adapter (plus addresses from the
//! config), then the household's rooms and groups from one speaker's ZoneGroupTopology.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, ToSocketAddrs};
use std::time::Duration;

use super::control::Speaker;
use crate::outputs::multicast_search;

/// Sonos speakers take UPnP control requests on this port.
pub const CONTROL_PORT: u16 = 1400;
const SSDP_GROUP: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 1900);
const SEARCH: &str = "M-SEARCH * HTTP/1.1\r\n\
                      HOST: 239.255.255.250:1900\r\n\
                      MAN: \"ssdp:discover\"\r\n\
                      MX: 1\r\n\
                      ST: urn:schemas-upnp-org:device:ZonePlayer:1\r\n\r\n";

/// A group of rooms playing together; a single room is a group of one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Zone {
    /// Room names, the coordinator's first.
    pub rooms: Vec<String>,
    /// The group coordinator, which takes the group's transport and volume commands.
    pub coordinator: SocketAddr,
    /// The coordinator's id (`RINCON_...`).
    pub uuid: String,
    /// Control addresses of every speaker in the group (pairs and surrounds included).
    pub members: Vec<SocketAddr>,
}

impl Zone {
    /// "Living Room", or "Living Room + Kitchen" for a group.
    pub fn name(&self) -> String {
        self.rooms.join(" + ")
    }
}

/// `ip`, `ip:port` or a host name; the port defaults to 1400.
fn host_addr(host: &str) -> Option<SocketAddr> {
    let host = host.trim();
    if host.is_empty() {
        return None;
    }
    if let Ok(addr) = host.parse::<SocketAddr>() {
        return Some(addr);
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Some(SocketAddr::new(ip, CONTROL_PORT));
    }
    let resolved = match host.rsplit_once(':') {
        Some((_, port)) if port.parse::<u16>().is_ok() => host.to_socket_addrs(),
        _ => (host, CONTROL_PORT).to_socket_addrs(),
    };
    resolved.ok()?.find(SocketAddr::is_ipv4)
}

/// `http://192.168.1.10:1400/xml/device_description.xml` -> `192.168.1.10:1400`.
fn location_addr(url: &str) -> Option<SocketAddr> {
    let rest = url.trim().strip_prefix("http://")?;
    let host = rest.split('/').next()?;
    host.parse().ok().or_else(|| {
        host.parse::<IpAddr>()
            .ok()
            .map(|ip| SocketAddr::new(ip, 80))
    })
}

/// The speaker address in an SSDP answer, if it comes from a Sonos speaker.
fn parse_ssdp_answer(text: &str) -> Option<SocketAddr> {
    let mut location = None;
    let mut sonos = false;
    for line in text.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "location" => location = location_addr(value),
            "st" | "nt" => sonos |= value.contains("ZonePlayer"),
            "server" => sonos |= value.contains("Sonos"),
            _ => {}
        }
    }
    location.filter(|_| sonos)
}

/// Sends an SSDP search for Sonos speakers and collects answers for `wait`.
fn ssdp_search(wait: Duration) -> Vec<SocketAddr> {
    let mut found = Vec::new();
    multicast_search(SSDP_GROUP, SEARCH.as_bytes(), 4, wait, |data, from| {
        let text = String::from_utf8_lossy(data);
        if let Some(addr) = parse_ssdp_answer(&text)
            && !found.contains(&addr)
        {
            log::debug!("Sonos speaker at {addr} (answer from {from})");
            found.push(addr);
        }
    });
    found
}

/// The groups in a `ZoneGroupState` document. Invisible members (the second speaker of a
/// stereo pair, subs, surrounds) and bridges are not listed as rooms.
pub fn parse_zone_groups(xml: &str) -> Vec<Zone> {
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return Vec::new();
    };
    let mut zones = Vec::new();
    for group in doc.descendants().filter(|n| n.has_tag_name("ZoneGroup")) {
        let Some(coordinator_id) = group.attribute("Coordinator") else {
            continue;
        };
        let members: Vec<_> = group
            .children()
            .filter(|n| n.has_tag_name("ZoneGroupMember"))
            .collect();
        let Some(coordinator) = members
            .iter()
            .find(|m| m.attribute("UUID") == Some(coordinator_id))
            .and_then(|m| location_addr(m.attribute("Location")?))
        else {
            continue;
        };
        let mut rooms: Vec<String> = Vec::new();
        // The coordinator's room first.
        let ordered = members
            .iter()
            .filter(|m| m.attribute("UUID") == Some(coordinator_id))
            .chain(
                members
                    .iter()
                    .filter(|m| m.attribute("UUID") != Some(coordinator_id)),
            );
        for m in ordered {
            let hidden =
                m.attribute("Invisible") == Some("1") || m.attribute("IsZoneBridge") == Some("1");
            if let Some(name) = m.attribute("ZoneName").filter(|_| !hidden)
                && !rooms.iter().any(|r| r == name)
            {
                rooms.push(name.to_string());
            }
        }
        if rooms.is_empty() {
            continue;
        }
        let mut speakers: Vec<SocketAddr> = Vec::new();
        for addr in group
            .descendants()
            .filter_map(|n| n.attribute("Location"))
            .filter_map(location_addr)
        {
            if !speakers.contains(&addr) {
                speakers.push(addr);
            }
        }
        zones.push(Zone {
            rooms,
            coordinator,
            uuid: coordinator_id.to_string(),
            members: speakers,
        });
    }
    zones
}

/// Finds the speaker groups on the network: by SSDP for `wait` (when `ssdp` is set) and at
/// the `hosts` from the config. An error means speakers answered but could not be asked
/// about their groups.
pub fn discover(hosts: &[String], ssdp: bool, wait: Duration) -> Result<Vec<Zone>, String> {
    let mut found: Vec<SocketAddr> = hosts.iter().filter_map(|h| host_addr(h)).collect();
    if ssdp {
        found.extend(ssdp_search(wait));
    }
    let mut zones: Vec<Zone> = Vec::new();
    let mut known: HashSet<SocketAddr> = HashSet::new();
    let mut errors = Vec::new();
    for addr in found {
        // One speaker describes its whole household.
        if !known.insert(addr) {
            continue;
        }
        match Speaker::new(addr).zone_group_state() {
            Ok(xml) => {
                for zone in parse_zone_groups(&xml) {
                    known.extend(zone.members.iter().copied());
                    if !zones.iter().any(|z| z.uuid == zone.uuid) {
                        zones.push(zone);
                    }
                }
            }
            Err(e) => {
                log::warn!("Sonos speaker at {addr}: {e}");
                errors.push(format!("{}: {e}", addr.ip()));
            }
        }
    }
    if zones.is_empty() && !errors.is_empty() {
        return Err(errors.join("; "));
    }
    zones.sort_by(|a, b| crate::library::natural_cmp(&a.name(), &b.name()));
    Ok(zones)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_sonos_ssdp_answers_only() {
        let sonos = "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age = 1800\r\nEXT:\r\n\
                     LOCATION: http://192.168.1.31:1400/xml/device_description.xml\r\n\
                     SERVER: Linux UPnP/1.0 Sonos/80.1-55014 (ZPS27)\r\n\
                     ST: urn:schemas-upnp-org:device:ZonePlayer:1\r\n\
                     USN: uuid:RINCON_48A6B8000000001400::urn:schemas-upnp-org:device:ZonePlayer:1\r\n\r\n";
        assert_eq!(
            parse_ssdp_answer(sonos),
            Some("192.168.1.31:1400".parse().unwrap())
        );
        let router = "HTTP/1.1 200 OK\r\nLOCATION: http://192.168.1.1:5000/rootDesc.xml\r\n\
                      SERVER: OpenWRT/18 UPnP/1.1 MiniUPnPd/2.0\r\n\
                      ST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n\r\n";
        assert_eq!(parse_ssdp_answer(router), None);
    }

    #[test]
    fn host_addresses_default_to_the_control_port() {
        assert_eq!(
            host_addr("192.168.1.20"),
            Some("192.168.1.20:1400".parse().unwrap())
        );
        assert_eq!(
            host_addr(" 10.0.0.5:1401 "),
            Some("10.0.0.5:1401".parse().unwrap())
        );
        assert_eq!(host_addr(""), None);
    }

    const TOPOLOGY: &str = r#"<ZoneGroupState><ZoneGroups>
        <ZoneGroup Coordinator="RINCON_B" ID="RINCON_B:12">
          <ZoneGroupMember UUID="RINCON_A" Location="http://192.168.1.30:1400/xml/device_description.xml" ZoneName="Kitchen"/>
          <ZoneGroupMember UUID="RINCON_B" Location="http://192.168.1.31:1400/xml/device_description.xml" ZoneName="Living Room">
            <Satellite UUID="RINCON_S" Location="http://192.168.1.35:1400/xml/device_description.xml" ZoneName="Living Room" Invisible="1"/>
          </ZoneGroupMember>
          <ZoneGroupMember UUID="RINCON_R" Location="http://192.168.1.32:1400/xml/device_description.xml" ZoneName="Living Room" Invisible="1"/>
        </ZoneGroup>
        <ZoneGroup Coordinator="RINCON_C" ID="RINCON_C:3">
          <ZoneGroupMember UUID="RINCON_C" Location="http://192.168.1.33:1400/xml/device_description.xml" ZoneName="Office &amp; Studio"/>
        </ZoneGroup>
        <ZoneGroup Coordinator="RINCON_BR" ID="RINCON_BR:1">
          <ZoneGroupMember UUID="RINCON_BR" Location="http://192.168.1.40:1400/xml/device_description.xml" ZoneName="BRIDGE" IsZoneBridge="1"/>
        </ZoneGroup>
      </ZoneGroups><VanishedDevices></VanishedDevices></ZoneGroupState>"#;

    #[test]
    fn reads_groups_coordinators_and_visible_rooms() {
        let zones = parse_zone_groups(TOPOLOGY);
        assert_eq!(zones.len(), 2, "the bridge is not a room: {zones:?}");
        assert_eq!(zones[0].name(), "Living Room + Kitchen");
        assert_eq!(zones[0].coordinator, "192.168.1.31:1400".parse().unwrap());
        assert_eq!(zones[0].uuid, "RINCON_B");
        assert_eq!(zones[0].members.len(), 4, "pair and surround included");
        assert_eq!(zones[1].name(), "Office & Studio");
        // Older firmware has <ZoneGroups> as the root.
        let old = TOPOLOGY
            .replace("<ZoneGroupState>", "")
            .replace("<VanishedDevices></VanishedDevices></ZoneGroupState>", "");
        assert_eq!(parse_zone_groups(&old), zones);
        assert!(parse_zone_groups("not xml").is_empty());
    }
}

#[cfg(test)]
mod fake_tests {
    use super::*;
    use crate::outputs::sonos::fake::FakeSonos;

    #[test]
    fn asks_configured_speakers_about_their_groups() {
        let a = FakeSonos::start("Living Room");
        let b = FakeSonos::start("Bedroom");
        let hosts = vec![a.addr.to_string(), b.addr.to_string(), String::new()];
        let zones = discover(&hosts, false, Duration::ZERO).unwrap();
        let names: Vec<String> = zones.iter().map(Zone::name).collect();
        assert_eq!(names, ["Bedroom", "Living Room"]);
        assert_eq!(zones[1], a.zone());
    }

    #[test]
    fn speakers_that_fail_are_an_error_only_when_nothing_was_found() {
        let a = FakeSonos::start("Office");
        a.state().fail.insert("GetZoneGroupState".into(), 501);
        let hosts = vec![a.addr.to_string()];
        let err = discover(&hosts, false, Duration::ZERO).unwrap_err();
        assert!(err.contains("UPnP error 501"), "{err}");
        assert_eq!(discover(&[], false, Duration::ZERO), Ok(Vec::new()));
    }
}
