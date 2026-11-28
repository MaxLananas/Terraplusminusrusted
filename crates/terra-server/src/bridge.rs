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
//!
//! # Trust
//!
//! The proxy and players share one road: a plugin message reaches this server through some
//! player's connection, and a modified client can write any bytes on any channel. Upstream
//! trusts every message; the port does not:
//!
//! * a delivery only carries `/tpll` coordinates, never `-p` or a selector (checked by the mod);
//! * a delivery for a player who is online is only honoured when it arrives through that
//!   player's own connection ([`decide_delivery`]): nobody can teleport someone else;
//! * deliveries parked for players who have not joined yet are bounded per carrying connection
//!   ([`PendingArrivals::park`]), so one client cannot push the real announcements out;
//! * a `GetServer` answer is only taken from a connection it was asked through, and the
//!   configured `linked_worlds.server_name` always wins over it ([`ServerName`]).

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

/// What to do with a [`BridgeDelivery`] for `target`, given the connection that carried it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryDecision {
    /// The target is here and sent it through their own connection: run `/tpll` now.
    RunNow,
    /// The target has not arrived yet (the proxy announces a player while handing them over):
    /// keep it for their join.
    Park,
    /// Refused, with the reason to log.
    Reject(&'static str),
}

/// Upstream runs any delivery for an online player. Here one for an online player must come
/// through that player's connection: the proxy sends it there once they are connected, while
/// another client sending it would be teleporting someone else.
pub fn decide_delivery(target: u128, target_online: bool, carrier: Option<u128>) -> DeliveryDecision {
    match (target_online, carrier) {
        (true, Some(carrier)) if carrier == target => DeliveryDecision::RunNow,
        (true, _) => DeliveryDecision::Reject("it is for another online player than the connection it came through"),
        (false, _) => DeliveryDecision::Park,
    }
}

/// Announcements one connection may have parked at once. The proxy announces each player as it
/// moves them, so even a busy carrier stays well below this; a client forging announcements
/// cannot fill the table.
pub const MAX_PARKED_PER_CARRIER: usize = 16;

/// `PlayerHashMapManagement`: teleports the proxy announced before the player joined. Bounded,
/// and entries expire, so a player who never arrives does not stay forever.
#[derive(Debug)]
pub struct PendingArrivals {
    /// Coordinates, when they were parked, and the connection that carried them.
    entries: HashMap<u128, (String, Instant, Option<u128>)>,
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

    /// Parks `coordinates` for `uuid`, carried by `carrier`'s connection, replacing an older
    /// announcement. When full, the oldest announcement makes room. False (nothing parked) when
    /// the carrier already has [`MAX_PARKED_PER_CARRIER`] announcements waiting.
    pub fn park(&mut self, uuid: u128, coordinates: String, carrier: Option<u128>, now: Instant) -> bool {
        self.expire(now);
        let replacing = self.entries.contains_key(&uuid);
        if !replacing && self.entries.values().filter(|(_, _, by)| *by == carrier).count() >= MAX_PARKED_PER_CARRIER {
            return false;
        }
        if !replacing
            && self.entries.len() >= self.capacity
            && let Some(oldest) = self.entries.iter().min_by_key(|(_, (_, at, _))| *at).map(|(uuid, _)| *uuid)
        {
            self.entries.remove(&oldest);
        }
        self.entries.insert(uuid, (coordinates, now, carrier));
        true
    }

    /// The announcement for a player who just joined, removed.
    pub fn take(&mut self, uuid: u128, now: Instant) -> Option<String> {
        self.expire(now);
        self.entries.remove(&uuid).map(|(coordinates, _, _)| coordinates)
    }

    fn expire(&mut self, now: Instant) {
        let lifetime = self.lifetime;
        self.entries.retain(|_, (_, at, _)| now.saturating_duration_since(*at) < lifetime);
    }
}

/// Players a `GetServer` request may be outstanding through at once.
pub const MAX_OUTSTANDING_QUERIES: usize = 64;

/// This server's name among the linked servers (`Terraplusminus.getRegisteredServerName`).
///
/// Upstream takes the first `GetServer` answer from anyone. Here an answer only counts when it
/// comes through a connection a request was sent through, and the configured
/// `linked_worlds.server_name` is never replaced by it (a mismatch is reported instead).
#[derive(Debug, Default)]
pub struct ServerName {
    configured: Option<String>,
    proxy: Option<String>,
    /// Connections a request went through and that have not answered.
    asked: Vec<u128>,
}

/// What became of a `GetServer` answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerNameAnswer {
    /// The first answer: the name is now known (or confirmed when configured).
    Registered(String),
    /// The proxy uses another name than the configured one, which is kept.
    Mismatch { proxy: String, configured: String },
    /// Already known, so ignored.
    Ignored,
    /// Not through a connection a request was sent through: refused.
    Unsolicited,
}

impl ServerName {
    pub fn configure(&mut self, configured: Option<String>) {
        self.configured = configured;
    }

    /// The configured name, else the proxy's answer.
    pub fn name(&self) -> Option<&str> {
        self.configured.as_deref().or(self.proxy.as_deref())
    }

    pub fn configured(&self) -> Option<&str> {
        self.configured.as_deref()
    }

    pub fn proxy(&self) -> Option<&str> {
        self.proxy.as_deref()
    }

    /// Whether the proxy still has to be asked.
    pub fn wanted(&self) -> bool {
        self.proxy.is_none()
    }

    /// Records that a request was sent through `player`'s connection.
    pub fn asked(&mut self, player: u128) {
        if !self.asked.contains(&player) {
            if self.asked.len() >= MAX_OUTSTANDING_QUERIES {
                self.asked.remove(0);
            }
            self.asked.push(player);
        }
    }

    /// `player` left: an answer can no longer come through their connection.
    pub fn forget(&mut self, player: u128) {
        self.asked.retain(|asked| *asked != player);
    }

    /// An answer naming this server `name`, arriving through `carrier`'s connection.
    pub fn answer(&mut self, name: String, carrier: Option<u128>) -> ServerNameAnswer {
        if !carrier.is_some_and(|carrier| self.asked.contains(&carrier)) {
            return ServerNameAnswer::Unsolicited;
        }
        if self.proxy.is_some() {
            return ServerNameAnswer::Ignored;
        }
        self.asked.clear();
        self.proxy = Some(name.clone());
        match &self.configured {
            Some(configured) if !configured.eq_ignore_ascii_case(&name) => ServerNameAnswer::Mismatch { proxy: name, configured: configured.clone() },
            _ => ServerNameAnswer::Registered(name),
        }
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
    fn deliveries_for_someone_else_online_are_refused() {
        assert_eq!(decide_delivery(7, true, Some(7)), DeliveryDecision::RunNow);
        assert!(matches!(decide_delivery(7, true, Some(8)), DeliveryDecision::Reject(_)), "a client teleporting another player");
        assert!(matches!(decide_delivery(7, true, None), DeliveryDecision::Reject(_)));
        assert_eq!(decide_delivery(7, false, Some(8)), DeliveryDecision::Park, "the proxy announces players before they arrive");
    }

    #[test]
    fn one_connection_cannot_flood_the_arrivals() {
        let start = Instant::now();
        let mut arrivals = PendingArrivals::new(1024, Duration::from_secs(60));
        assert!(arrivals.park(1, "1 1".into(), Some(500), start), "the proxy's announcement");
        for forged in 0..1000u128 {
            arrivals.park(10_000 + forged, "0 0".into(), Some(666), start);
        }
        assert_eq!(arrivals.len(), 1 + MAX_PARKED_PER_CARRIER);
        assert!(arrivals.park(10_000, "5 5".into(), Some(666), start), "replacing one of its own is fine");
        assert_eq!(arrivals.take(1, start).as_deref(), Some("1 1"), "the real announcement survived");
    }

    #[test]
    fn server_name_answers_are_only_taken_when_asked() {
        let mut name = ServerName::default();
        assert_eq!(name.answer("evil".into(), Some(9)), ServerNameAnswer::Unsolicited);
        assert_eq!(name.name(), None);
        name.asked(1);
        assert_eq!(name.answer("evil".into(), Some(9)), ServerNameAnswer::Unsolicited, "another connection");
        assert_eq!(name.answer("lobby".into(), Some(1)), ServerNameAnswer::Registered("lobby".into()));
        assert_eq!(name.answer("other".into(), Some(1)), ServerNameAnswer::Unsolicited, "one answer per request");
        assert_eq!(name.name(), Some("lobby"));
        assert!(!name.wanted());

        let mut configured = ServerName::default();
        configured.configure(Some("Mid".into()));
        configured.asked(2);
        configured.forget(2);
        assert_eq!(configured.answer("x".into(), Some(2)), ServerNameAnswer::Unsolicited, "the player left");
        configured.asked(3);
        assert_eq!(configured.answer("high".into(), Some(3)), ServerNameAnswer::Mismatch { proxy: "high".into(), configured: "Mid".into() });
        assert_eq!(configured.name(), Some("Mid"), "the configuration wins");
        for player in 0..1000 {
            configured.asked(player);
        }
        assert!(configured.asked.len() <= MAX_OUTSTANDING_QUERIES);
    }

    #[test]
    fn arrivals_are_bounded_and_expire() {
        let start = Instant::now();
        let mut arrivals = PendingArrivals::new(2, Duration::from_secs(60));
        arrivals.park(1, "1 1".into(), None, start);
        arrivals.park(2, "2 2".into(), None, start + Duration::from_secs(1));
        arrivals.park(1, "1 9".into(), None, start + Duration::from_secs(2));
        assert_eq!(arrivals.len(), 2);
        arrivals.park(3, "3 3".into(), None, start + Duration::from_secs(3));
        assert_eq!(arrivals.len(), 2, "the oldest made room");
        assert_eq!(arrivals.take(2, start + Duration::from_secs(3)), None);
        assert_eq!(arrivals.take(1, start + Duration::from_secs(4)).as_deref(), Some("1 9"));
        assert_eq!(arrivals.take(1, start + Duration::from_secs(4)), None, "taken once");
        assert_eq!(arrivals.take(3, start + Duration::from_secs(64)), None, "expired");
    }
}
