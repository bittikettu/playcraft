//! Finding Google Cast devices: an mDNS query for `_googlecast._tcp.local`, which every
//! Android TV, Chromecast and Nest speaker on the network answers.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use crate::outputs::multicast_search;

const MDNS_GROUP: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(224, 0, 0, 251), 5353);
const SERVICE: &str = "_googlecast._tcp.local";
/// Cast devices take connections on this port unless they say otherwise.
pub const CAST_PORT: u16 = 8009;

const TYPE_A: u16 = 1;
const TYPE_PTR: u16 = 12;
const TYPE_TXT: u16 = 16;
const TYPE_SRV: u16 = 33;

/// A Cast device on the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CastDevice {
    /// The name set in the Google Home app, e.g. "Living Room TV".
    pub name: String,
    /// The model, e.g. "Chromecast" or "Smart TV".
    pub model: String,
    pub id: String,
    pub addr: SocketAddr,
}

/// A PTR question for the Cast service, asking for answers sent straight back to us.
fn query() -> Vec<u8> {
    // Id 0, no flags, one question.
    let mut q = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in SERVICE.split('.') {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&TYPE_PTR.to_be_bytes());
    // Class IN, with the "unicast response" bit.
    q.extend_from_slice(&0x8001u16.to_be_bytes());
    q
}

fn be16(packet: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*packet.get(at)?, *packet.get(at + 1)?]))
}

/// Reads a (possibly compressed) name at `pos`. Returns it and the position after it.
fn read_name(packet: &[u8], mut pos: usize) -> Option<(String, usize)> {
    let mut labels = Vec::new();
    let mut after = None;
    let mut jumps = 0;
    loop {
        let len = *packet.get(pos)? as usize;
        if len == 0 {
            pos += 1;
            break;
        }
        if len & 0xC0 == 0xC0 {
            jumps += 1;
            if jumps > 32 {
                return None;
            }
            after.get_or_insert(pos + 2);
            pos = ((len & 0x3F) << 8) | *packet.get(pos + 1)? as usize;
            continue;
        }
        let label = packet.get(pos + 1..pos + 1 + len)?;
        labels.push(String::from_utf8_lossy(label).into_owned());
        pos += 1 + len;
    }
    Some((labels.join("."), after.unwrap_or(pos)))
}

struct Record<'a> {
    name: String,
    kind: u16,
    data: &'a [u8],
    /// Where `data` starts in the packet (names in it may point back into the packet).
    at: usize,
}

fn records(packet: &[u8]) -> Option<Vec<Record<'_>>> {
    let questions = be16(packet, 4)?;
    let answers =
        be16(packet, 6)? as usize + be16(packet, 8)? as usize + be16(packet, 10)? as usize;
    let mut pos = 12;
    for _ in 0..questions {
        pos = read_name(packet, pos)?.1 + 4;
    }
    let mut out = Vec::new();
    for _ in 0..answers {
        let (name, after) = read_name(packet, pos)?;
        let kind = be16(packet, after)?;
        let len = be16(packet, after + 8)? as usize;
        let at = after + 10;
        let data = packet.get(at..at + len)?;
        out.push(Record {
            name,
            kind,
            data,
            at,
        });
        pos = at + len;
    }
    Some(out)
}

/// `key=value` strings of a TXT record, keys in lowercase.
fn txt_values(data: &[u8]) -> HashMap<String, String> {
    let mut values = HashMap::new();
    let mut i = 0;
    while let Some(&len) = data.get(i) {
        let Some(item) = data.get(i + 1..i + 1 + len as usize) else {
            break;
        };
        let item = String::from_utf8_lossy(item);
        if let Some((key, value)) = item.split_once('=') {
            values.insert(key.to_ascii_lowercase(), value.to_string());
        }
        i += 1 + len as usize;
    }
    values
}

/// The Cast devices described in an mDNS answer that came from `from`.
pub fn parse_answer(packet: &[u8], from: IpAddr) -> Vec<CastDevice> {
    let Some(records) = records(packet) else {
        return Vec::new();
    };
    let suffix = format!(".{SERVICE}");
    let mut instances: Vec<String> = Vec::new();
    let mut txt: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut srv: HashMap<String, (u16, String)> = HashMap::new();
    let mut hosts: HashMap<String, Ipv4Addr> = HashMap::new();
    for r in &records {
        let name = r.name.to_ascii_lowercase();
        match r.kind {
            TYPE_PTR if name == SERVICE => {
                if let Some((instance, _)) = read_name(packet, r.at) {
                    instances.push(instance.to_ascii_lowercase());
                }
            }
            TYPE_TXT => {
                txt.insert(name, txt_values(r.data));
            }
            TYPE_SRV if r.data.len() > 6 => {
                if let (Some(port), Some((target, _))) =
                    (be16(r.data, 4), read_name(packet, r.at + 6))
                {
                    srv.insert(name, (port, target.to_ascii_lowercase()));
                }
            }
            TYPE_A if r.data.len() == 4 => {
                hosts.insert(
                    name,
                    Ipv4Addr::new(r.data[0], r.data[1], r.data[2], r.data[3]),
                );
            }
            _ => {}
        }
    }
    // Records for an instance without the PTR that names it still describe a device.
    for name in txt.keys().chain(srv.keys()) {
        if name.ends_with(&suffix) && !instances.contains(name) {
            instances.push(name.clone());
        }
    }
    instances
        .into_iter()
        .map(|instance| {
            let values = txt.remove(&instance).unwrap_or_default();
            let (port, host) = srv
                .get(&instance)
                .cloned()
                .unwrap_or((CAST_PORT, String::new()));
            let ip = hosts.get(&host).map_or(from, |&ip| IpAddr::V4(ip));
            let label = instance
                .strip_suffix(&suffix)
                .unwrap_or(&instance)
                .to_string();
            CastDevice {
                name: values.get("fn").cloned().unwrap_or_else(|| label.clone()),
                model: values.get("md").cloned().unwrap_or_default(),
                id: values.get("id").cloned().unwrap_or(label),
                addr: SocketAddr::new(ip, port),
            }
        })
        .collect()
}

/// Asks the network for Cast devices and collects answers for `wait`.
pub fn discover(wait: Duration) -> Vec<CastDevice> {
    let mut found: Vec<CastDevice> = Vec::new();
    multicast_search(MDNS_GROUP, &query(), 255, wait, |packet, from| {
        for device in parse_answer(packet, from.ip()) {
            if !found.iter().any(|d| d.id == device.id) {
                log::debug!("Cast device {} at {}", device.name, device.addr);
                found.push(device);
            }
        }
    });
    found
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn name(out: &mut Vec<u8>, name: &str) {
        for label in name.split('.') {
            out.push(label.len() as u8);
            out.extend_from_slice(label.as_bytes());
        }
        out.push(0);
    }

    fn record(out: &mut Vec<u8>, owner: &[u8], kind: u16, data: &[u8]) {
        out.extend_from_slice(owner);
        out.extend_from_slice(&kind.to_be_bytes());
        out.extend_from_slice(&0x8001u16.to_be_bytes());
        out.extend_from_slice(&120u32.to_be_bytes());
        out.extend_from_slice(&(data.len() as u16).to_be_bytes());
        out.extend_from_slice(data);
    }

    /// An answer like a TV gives: PTR, then TXT, SRV and A in the additional records, with
    /// names compressed by pointers.
    pub(crate) fn tv_answer(friendly: &str, ip: [u8; 4], port: u16) -> Vec<u8> {
        let mut p = vec![0, 0, 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 3];
        // The PTR record's owner is the service name, written out in full.
        let service_at = p.len() as u16;
        let mut service = Vec::new();
        name(&mut service, SERVICE);
        let label = b"Smart-TV-0123456789abcdef";
        let mut ptr_data = vec![label.len() as u8];
        ptr_data.extend_from_slice(label);
        ptr_data.extend_from_slice(&(0xC000 | service_at).to_be_bytes());
        // The instance name starts the PTR record's data, after owner, type, class, TTL
        // and length.
        let instance_at = (p.len() + service.len() + 10) as u16;
        record(&mut p, &service, TYPE_PTR, &ptr_data);
        let instance = (0xC000 | instance_at).to_be_bytes();
        let mut txt = Vec::new();
        for item in [
            "id=0123456789abcdef".to_string(),
            "md=Smart TV".to_string(),
            format!("fn={friendly}"),
            "rs=Netflix".to_string(),
        ] {
            txt.push(item.len() as u8);
            txt.extend_from_slice(item.as_bytes());
        }
        record(&mut p, &instance, TYPE_TXT, &txt);
        let mut srv = vec![0, 0, 0, 0];
        srv.extend_from_slice(&port.to_be_bytes());
        let mut host = Vec::new();
        name(&mut host, "0123-4567.local");
        srv.extend_from_slice(&host);
        record(&mut p, &instance, TYPE_SRV, &srv);
        record(&mut p, &host, TYPE_A, &ip);
        p
    }

    #[test]
    fn asks_for_cast_devices() {
        let q = query();
        assert_eq!(&q[4..6], &[0, 1], "one question");
        assert!(q.windows(11).any(|w| w == b"_googlecast"));
        assert_eq!(&q[q.len() - 4..], &[0, 12, 0x80, 1]);
    }

    #[test]
    fn reads_devices_from_answers() {
        let from: IpAddr = "192.168.1.99".parse().unwrap();
        let devices = parse_answer(&tv_answer("Televisiö", [192, 168, 1, 201], 8009), from);
        assert_eq!(
            devices,
            [CastDevice {
                name: "Televisiö".into(),
                model: "Smart TV".into(),
                id: "0123456789abcdef".into(),
                addr: "192.168.1.201:8009".parse().unwrap(),
            }]
        );
        // A speaker group answers on a port of its own.
        let group = parse_answer(&tv_answer("Downstairs", [10, 0, 0, 5], 32187), from);
        assert_eq!(group[0].addr, "10.0.0.5:32187".parse().unwrap());
        // Rubbish is ignored.
        assert!(parse_answer(&[1, 2, 3], from).is_empty());
        let mut cut = tv_answer("TV", [10, 0, 0, 5], 8009);
        cut.truncate(cut.len() - 3);
        assert!(parse_answer(&cut, from).len() <= 1);
    }
}
