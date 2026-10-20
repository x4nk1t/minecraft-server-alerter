//! Minecraft Java Edition "Server List Ping" — the same status request the
//! multiplayer screen sends. See https://minecraft.wiki/w/Java_Edition_protocol/Server_List_Ping

use std::io::{self, BufReader, ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, ensure};
use serde_json::Value;

/// Any protocol version works for a status request; 47 (1.8) is the long-standing convention.
const PROTOCOL_VERSION: i32 = 47;
/// Status JSON is usually a few KB (more with a favicon). Anything past this is not a real server.
const MAX_PACKET: usize = 2 * 1024 * 1024;

pub struct Status {
    /// The host:port actually pinged, after following any SRV record.
    pub endpoint: String,
    pub version: String,
    pub players_online: i64,
    pub players_max: i64,
    pub latency: Duration,
}

/// Connects to `host:port` and asks for its status. `handshake_host` is the name the
/// player typed, which proxies like Velocity/BungeeCord use for forced hosts.
pub fn ping(host: &str, port: u16, handshake_host: &str, timeout: Duration) -> Result<Status> {
    let addrs: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("resolving {host}"))?
        .collect();

    let start = Instant::now();
    let mut last_err = anyhow!("{host} resolved to no addresses");
    let mut connected = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(stream) => {
                connected = Some(stream);
                break;
            }
            Err(e) => last_err = describe_connect(e, addr, timeout),
        }
    }
    let stream = connected.ok_or(last_err)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;

    let mut handshake = Vec::with_capacity(32 + handshake_host.len());
    write_varint(&mut handshake, 0x00); // packet id: handshake
    write_varint(&mut handshake, PROTOCOL_VERSION);
    write_varint(&mut handshake, handshake_host.len() as i32);
    handshake.extend_from_slice(handshake_host.as_bytes());
    handshake.extend_from_slice(&port.to_be_bytes());
    write_varint(&mut handshake, 1); // next state: status

    let mut out = Vec::with_capacity(handshake.len() + 8);
    write_packet(&mut out, &handshake);
    write_packet(&mut out, &[0x00]); // status request
    (&stream)
        .write_all(&out)
        .map_err(|e| describe_io(e, timeout))
        .context("sending status request")?;

    let mut reader = BufReader::new(&stream);
    let len = read_varint(&mut reader).map_err(|e| describe_io(e, timeout))?;
    ensure!(
        len > 0 && len as usize <= MAX_PACKET,
        "server sent a bogus packet length ({len})"
    );
    let mut packet = vec![0; len as usize];
    reader
        .read_exact(&mut packet)
        .map_err(|e| describe_io(e, timeout))?;
    let latency = start.elapsed();

    let mut body = &packet[..];
    let packet_id = read_varint(&mut body)?;
    ensure!(
        packet_id == 0,
        "expected status response, got packet 0x{packet_id:02x}"
    );
    let json_len = read_varint(&mut body)?;
    let json = usize::try_from(json_len)
        .ok()
        .and_then(|n| body.get(..n))
        .context("status response is truncated")?;
    let json: Value = serde_json::from_slice(json).context("server sent invalid status JSON")?;

    Ok(Status {
        endpoint: format!("{host}:{port}"),
        version: strip_formatting(json["version"]["name"].as_str().unwrap_or("unknown")),
        players_online: json["players"]["online"].as_i64().unwrap_or(0),
        players_max: json["players"]["max"].as_i64().unwrap_or(0),
        latency,
    })
}

fn describe_connect(e: io::Error, addr: SocketAddr, timeout: Duration) -> anyhow::Error {
    match e.kind() {
        ErrorKind::ConnectionRefused => {
            anyhow!("{addr} refused the connection (nothing is listening on that port)")
        }
        ErrorKind::TimedOut | ErrorKind::WouldBlock => {
            anyhow!("{addr} didn't answer within {}s", timeout.as_secs())
        }
        ErrorKind::HostUnreachable | ErrorKind::NetworkUnreachable => {
            anyhow!("{addr} is unreachable")
        }
        _ => anyhow!(e).context(format!("connecting to {addr}")),
    }
}

fn describe_io(e: io::Error, timeout: Duration) -> anyhow::Error {
    match e.kind() {
        ErrorKind::WouldBlock | ErrorKind::TimedOut => {
            anyhow!(
                "server accepted the connection but didn't answer within {}s",
                timeout.as_secs()
            )
        }
        ErrorKind::UnexpectedEof => anyhow!("server closed the connection without answering"),
        _ => e.into(),
    }
}

/// Removes `§x` color/style codes that some servers put in their version string.
fn strip_formatting(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '§' {
            chars.next();
        } else {
            out.push(c);
        }
    }
    out.trim().to_owned()
}

fn write_packet(out: &mut Vec<u8>, body: &[u8]) {
    write_varint(out, body.len() as i32);
    out.extend_from_slice(body);
}

fn write_varint(out: &mut Vec<u8>, value: i32) {
    let mut v = value as u32;
    loop {
        if v & !0x7F == 0 {
            out.push(v as u8);
            return;
        }
        out.push((v & 0x7F) as u8 | 0x80);
        v >>= 7;
    }
}

fn read_varint(r: &mut impl Read) -> io::Result<i32> {
    let mut result = 0u32;
    for i in 0..5 {
        let mut byte = [0u8];
        r.read_exact(&mut byte)?;
        result |= u32::from(byte[0] & 0x7F) << (7 * i);
        if byte[0] & 0x80 == 0 {
            return Ok(result as i32);
        }
    }
    Err(io::Error::new(ErrorKind::InvalidData, "VarInt is too long"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_roundtrip() {
        for (value, bytes) in [
            (0, &[0x00][..]),
            (1, &[0x01]),
            (127, &[0x7f]),
            (128, &[0x80, 0x01]),
            (25565, &[0xdd, 0xc7, 0x01]),
            (2147483647, &[0xff, 0xff, 0xff, 0xff, 0x07]),
            (-1, &[0xff, 0xff, 0xff, 0xff, 0x0f]),
        ] {
            let mut out = Vec::new();
            write_varint(&mut out, value);
            assert_eq!(out, bytes, "encoding {value}");
            assert_eq!(
                read_varint(&mut &out[..]).unwrap(),
                value,
                "decoding {value}"
            );
        }
    }

    #[test]
    fn strips_formatting_codes() {
        assert_eq!(
            strip_formatting("§f§f§fWe support: §c1.20-1.21"),
            "We support: 1.20-1.21"
        );
        assert_eq!(strip_formatting("Paper 1.21.4"), "Paper 1.21.4");
    }

    #[test]
    fn varint_rejects_overlong() {
        assert!(read_varint(&mut &[0xff, 0xff, 0xff, 0xff, 0xff, 0x01][..]).is_err());
    }
}
