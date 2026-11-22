//! Linked-worlds method SERVER: the plugin messages upstream exchanges with
//! TerraPlusMinusProxyBridge on a BungeeCord or Velocity proxy, byte for byte.
//!
//! * `terraplusminus:teleportbridge`, server to proxy (`sendPluginMessageToBungeeBridge`):
//!   `writeUTF(uuid)`, `writeUTF("<server>, <offset>")`, `writeUTF("<lat>, <lng>")`. The proxy
//!   moves the player to `<server>` and sends that server, on the same channel,
//!   `writeUTF(uuid)`, `writeUTF(coordinates)` (`PluginMessageEvent`), which runs
//!   `tpll <coordinates>` for the player now or when they join (`PlayerJoinEvent`).
//! * `bungeecord:main` (`BungeeCord`): `writeUTF("GetServer")` asks the proxy for this server's
//!   name; the answer is `writeUTF("GetServer")`, `writeUTF(name)`.
//!
//! Strings are Java's `DataOutput.writeUTF`: a big-endian `u16` byte length and modified UTF-8.

use crate::coordinates::LatLng;
use crate::java_text::java_double_to_string;
use crate::tpll::LinkedWorlds;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// `Properties.NonConfigurable.CROSS_TELEPORTATION_CHANNEL`.
pub const TELEPORT_BRIDGE_CHANNEL: &str = "terraplusminus:teleportbridge";
/// The BungeeCord plugin channel, as named since 1.13 (Velocity answers it too).
pub const BUNGEECORD_CHANNEL: &str = "bungeecord:main";
/// Upstream asks for the server name 20 ticks after a join.
pub const GET_SERVER_DELAY_TICKS: u64 = 20;

/// Java's `DataOutput.writeUTF`. `None` when the encoding is longer than 65535 bytes, where Java
/// throws `UTFDataFormatException`.
pub fn write_java_utf(out: &mut Vec<u8>, text: &str) -> Option<()> {
    let mut encoded = Vec::with_capacity(text.len());
    for unit in text.encode_utf16() {
        match unit {
            0x0001..=0x007F => encoded.push(unit as u8),
            0x0000 | 0x0080..=0x07FF => {
                encoded.push(0xC0 | ((unit >> 6) & 0x1F) as u8);
                encoded.push(0x80 | (unit & 0x3F) as u8);
            }
            _ => {
                encoded.push(0xE0 | ((unit >> 12) & 0x0F) as u8);
                encoded.push(0x80 | ((unit >> 6) & 0x3F) as u8);
                encoded.push(0x80 | (unit & 0x3F) as u8);
            }
        }
    }
    let length = u16::try_from(encoded.len()).ok()?;
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(&encoded);
    Some(())
}

/// Java's `DataInput.readUTF` from the front of `input`, advancing it. `None` for a truncated or
/// malformed encoding, where Java throws.
pub fn read_java_utf(input: &mut &[u8]) -> Option<String> {
    let (length, rest) = input.split_first_chunk::<2>()?;
    let length = usize::from(u16::from_be_bytes(*length));
    let bytes = rest.get(..length)?;
    *input = rest.get(length..)?;
    let mut units = Vec::with_capacity(length);
    let mut index = 0;
    while index < bytes.len() {
        let first = u16::from(bytes[index]);
        let continuation = |offset: usize| bytes.get(index + offset).copied().filter(|byte| byte & 0xC0 == 0x80).map(|byte| u16::from(byte & 0x3F));
        let (unit, width) = match first >> 4 {
            0..=7 => (first, 1),
            12 | 13 => ((first & 0x1F) << 6 | continuation(1)?, 2),
            14 => ((first & 0x0F) << 12 | continuation(1)? << 6 | continuation(2)?, 3),
            _ => return None,
        };
        units.push(unit);
        index += width;
    }
    // Java keeps unpaired surrogates; a Rust string cannot, so they are refused like garbage.
    String::from_utf16(&units).ok()
}

/// `UUID.toString`.
pub fn uuid_to_string(uuid: u128) -> String {
    let hex = format!("{uuid:032x}");
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

/// `UUID.fromString` for the canonical form (any case). Java's leniency about shortened groups
/// is not needed: the proxy echoes the string this server sent.
pub fn parse_uuid(text: &str) -> Option<u128> {
    let groups: Vec<&str> = text.split('-').collect();
    if groups.len() != 5 || groups.iter().zip([8, 4, 4, 4, 12]).any(|(group, length)| group.len() != length || !group.bytes().all(|b| b.is_ascii_hexdigit())) {
        return None;
    }
    u128::from_str_radix(&groups.concat(), 16).ok()
}

/// The proxy request for moving `uuid` to `server` (`<name>, <offset>`) at `position`.
pub fn teleport_request(uuid: u128, server: &str, offset: i32, position: LatLng) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    write_java_utf(&mut out, &uuid_to_string(uuid))?;
    write_java_utf(&mut out, &format!("{server}, {offset}"))?;
    write_java_utf(&mut out, &format!("{}, {}", java_double_to_string(position.latitude), java_double_to_string(position.longitude)))?;
    Some(out)
}

/// What the proxy delivers on [`TELEPORT_BRIDGE_CHANNEL`]: the player and the `/tpll` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeDelivery {
    pub uuid: u128,
    pub coordinates: String,
}

pub fn parse_delivery(mut bytes: &[u8]) -> Option<BridgeDelivery> {
    let uuid = parse_uuid(&read_java_utf(&mut bytes)?)?;
    let coordinates = read_java_utf(&mut bytes)?;
    Some(BridgeDelivery { uuid, coordinates })
}

/// `writeUTF("GetServer")`.
pub fn get_server_request() -> Vec<u8> {
    let mut out = Vec::new();
    let _ = write_java_utf(&mut out, "GetServer");
    out
}

/// The server name in a `GetServer` answer; other BungeeCord subchannels are not for us.
pub fn parse_get_server(mut bytes: &[u8]) -> Option<String> {
    (read_java_utf(&mut bytes)? == "GetServer").then(|| read_java_utf(&mut bytes)).flatten().filter(|name| !name.is_empty())
}

/// `ConfigurationHelper.getNextServerName` / `getPreviousServerName`: the server above or below
/// `current` in the list, matched without case, with its offset.
pub fn neighbour_server(linked: &LinkedWorlds, current: &str, upward: bool) -> Option<(String, i32)> {
    let index = linked.index_of(current)?;
    let neighbour = if upward { linked.worlds.get(index + 1) } else { linked.worlds.get(index.checked_sub(1)?) }?;
    Some((neighbour.name.clone(), neighbour.offset))
}

/// `PlayerHashMapManagement`: teleports the proxy announced before the player joined. Bounded,
/// and entries expire, so a player who never arrives does not stay forever.
#[derive(Debug)]
pub struct PendingArrivals {
    entries: HashMap<u128, (String, Instant)>,
    capacity: usize,
    lifetime: Duration,
}

impl PendingArrivals {
    pub fn new(capacity: usize, lifetime: Duration) -> Self {
        Self { entries: HashMap::new(), capacity: capacity.max(1), lifetime }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Parks `coordinates` for `uuid`, replacing an older announcement. When full, the oldest
    /// announcement makes room.
    pub fn park(&mut self, uuid: u128, coordinates: String, now: Instant) {
        self.expire(now);
        if !self.entries.contains_key(&uuid)
            && self.entries.len() >= self.capacity
            && let Some(oldest) = self.entries.iter().min_by_key(|(_, (_, at))| *at).map(|(uuid, _)| *uuid)
        {
            self.entries.remove(&oldest);
        }
        self.entries.insert(uuid, (coordinates, now));
    }

    /// The announcement for a player who just joined, removed.
    pub fn take(&mut self, uuid: u128, now: Instant) -> Option<String> {
        self.expire(now);
        self.entries.remove(&uuid).map(|(coordinates, _)| coordinates)
    }

    fn expire(&mut self, now: Instant) {
        let lifetime = self.lifetime;
        self.entries.retain(|_, (_, at)| now.saturating_duration_since(*at) < lifetime);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tpll::{LinkedWorld, LinkedWorldsMethod};

    #[test]
    fn java_utf_round_trips_like_data_output() {
        let mut out = Vec::new();
        write_java_utf(&mut out, "GetServer").unwrap();
        assert_eq!(out, b"\x00\x09GetServer");
        // Modified UTF-8: NUL takes two bytes, a supplementary character two 3-byte surrogates.
        let mut special = Vec::new();
        write_java_utf(&mut special, "\0é😀").unwrap();
        assert_eq!(special, [0x00, 0x0A, 0xC0, 0x80, 0xC3, 0xA9, 0xED, 0xA0, 0xBD, 0xED, 0xB8, 0x80]);
        let mut input = special.as_slice();
        assert_eq!(read_java_utf(&mut input).as_deref(), Some("\0é😀"));
        assert!(input.is_empty());
        assert!(write_java_utf(&mut Vec::new(), &"x".repeat(65_536)).is_none());
        assert!(write_java_utf(&mut Vec::new(), &"x".repeat(65_535)).is_some());
        for broken in [&b"\x00"[..], b"\x00\x05abc", b"\x00\x01\xC3", b"\x00\x02\xC3\x29", b"\x00\x01\xF0"] {
            assert_eq!(read_java_utf(&mut &broken[..]), None, "{broken:?}");
        }
    }

    #[test]
    fn uuids_use_java_text() {
        let uuid = 0x123e4567_e89b_12d3_a456_426614174000_u128;
        assert_eq!(uuid_to_string(uuid), "123e4567-e89b-12d3-a456-426614174000");
        assert_eq!(parse_uuid("123E4567-E89B-12D3-A456-426614174000"), Some(uuid));
        assert_eq!(uuid_to_string(0), "00000000-0000-0000-0000-000000000000");
        for bad in ["", "123e4567e89b12d3a456426614174000", "123e4567-e89b-12d3-a456-42661417400g", "123e4567-e89b-12d3-a456-4266141740000"] {
            assert_eq!(parse_uuid(bad), None, "{bad}");
        }
    }

    #[test]
    fn bridge_messages_match_upstream_layout() {
        let uuid = 0x123e4567_e89b_12d3_a456_426614174000_u128;
        let request = teleport_request(uuid, "lobby-high", -2032, LatLng { latitude: 48.0, longitude: 2.5 }).unwrap();
        let mut input = request.as_slice();
        assert_eq!(read_java_utf(&mut input).as_deref(), Some("123e4567-e89b-12d3-a456-426614174000"));
        assert_eq!(read_java_utf(&mut input).as_deref(), Some("lobby-high, -2032"));
        // `geoCoords.getLat() + ", " + geoCoords.getLng()`: Java double text.
        assert_eq!(read_java_utf(&mut input).as_deref(), Some("48.0, 2.5"));
        assert!(input.is_empty());

        let mut delivery = Vec::new();
        write_java_utf(&mut delivery, "123e4567-e89b-12d3-a456-426614174000").unwrap();
        write_java_utf(&mut delivery, "48.0, 2.5").unwrap();
        assert_eq!(parse_delivery(&delivery), Some(BridgeDelivery { uuid, coordinates: "48.0, 2.5".into() }));
        assert_eq!(parse_delivery(&delivery[..delivery.len() - 1]), None);

        let mut answer = get_server_request();
        assert_eq!(parse_get_server(&answer), None, "the request alone names no server");
        write_java_utf(&mut answer, "earth-low").unwrap();
        assert_eq!(parse_get_server(&answer).as_deref(), Some("earth-low"));
        let mut other = Vec::new();
        write_java_utf(&mut other, "PlayerCount").unwrap();
        write_java_utf(&mut other, "ALL").unwrap();
        assert_eq!(parse_get_server(&other), None);
    }

    #[test]
    fn neighbours_follow_the_configured_order() {
        let linked = LinkedWorlds {
            enabled: true,
            method: LinkedWorldsMethod::Servers,
            worlds: vec![LinkedWorld { name: "low".into(), offset: 2032 }, LinkedWorld { name: "mid".into(), offset: 0 }, LinkedWorld { name: "high".into(), offset: -2032 }],
        };
        assert_eq!(neighbour_server(&linked, "MID", true), Some(("high".into(), -2032)));
        assert_eq!(neighbour_server(&linked, "mid", false), Some(("low".into(), 2032)));
        assert_eq!(neighbour_server(&linked, "high", true), None);
        assert_eq!(neighbour_server(&linked, "low", false), None);
        assert_eq!(neighbour_server(&linked, "elsewhere", true), None);
    }

    #[test]
    fn arrivals_are_bounded_and_expire() {
        let start = Instant::now();
        let mut arrivals = PendingArrivals::new(2, Duration::from_secs(60));
        arrivals.park(1, "1 1".into(), start);
        arrivals.park(2, "2 2".into(), start + Duration::from_secs(1));
        arrivals.park(1, "1 9".into(), start + Duration::from_secs(2));
        assert_eq!(arrivals.len(), 2);
        arrivals.park(3, "3 3".into(), start + Duration::from_secs(3));
        assert_eq!(arrivals.len(), 2, "the oldest made room");
        assert_eq!(arrivals.take(2, start + Duration::from_secs(3)), None);
        assert_eq!(arrivals.take(1, start + Duration::from_secs(4)).as_deref(), Some("1 9"));
        assert_eq!(arrivals.take(1, start + Duration::from_secs(4)), None, "taken once");
        assert_eq!(arrivals.take(3, start + Duration::from_secs(64)), None, "expired");
    }
}
