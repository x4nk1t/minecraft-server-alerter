//! Minimal DNS client for the `_minecraft._tcp` SRV record, so we connect to the same
//! host/port the game client would when a player types an address without a port.
//! Hand-rolled to avoid pulling an async runtime into a binary that runs every 10 minutes.

use std::cmp::Reverse;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail, ensure};

const TYPE_SRV: u16 = 33;
const CLASS_IN: u16 = 1;
const FLAG_RESPONSE: u16 = 0x8000;
const FLAG_TRUNCATED: u16 = 0x0200;
const RCODE_NXDOMAIN: u16 = 3;

/// Returns the SRV target for `host`, or `None` if it has no record (connect to it directly on 25565).
pub fn lookup_minecraft(host: &str, timeout: Duration) -> Result<Option<(String, u16)>> {
    if host.parse::<IpAddr>().is_ok() {
        return Ok(None);
    }
    let name = format!("_minecraft._tcp.{}", host.trim_end_matches('.'));
    let id = query_id();
    let query = build_query(id, &name)?;

    let mut last_err = anyhow!("no DNS servers configured");
    for ns in nameservers() {
        match exchange(ns, &query, id, timeout).and_then(|resp| parse_response(&resp, id)) {
            Ok(answer) => return Ok(answer),
            Err(e) => last_err = e.context(format!("asking DNS server {ns}")),
        }
    }
    Err(last_err.context(format!("SRV lookup for {name}")))
}

fn nameservers() -> Vec<SocketAddr> {
    let servers: Vec<SocketAddr> = fs::read_to_string("/etc/resolv.conf")
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.trim().strip_prefix("nameserver"))
        .filter_map(|ip| ip.trim().parse::<IpAddr>().ok())
        .map(|ip| SocketAddr::new(ip, 53))
        .collect();
    if servers.is_empty() {
        vec![
            SocketAddr::new(Ipv4Addr::new(1, 1, 1, 1).into(), 53),
            SocketAddr::new(Ipv4Addr::new(8, 8, 8, 8).into(), 53),
        ]
    } else {
        servers
    }
}

fn query_id() -> u16 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    (nanos ^ std::process::id()) as u16
}

fn build_query(id: u16, name: &str) -> Result<Vec<u8>> {
    let mut q = Vec::with_capacity(18 + name.len());
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&0x0100u16.to_be_bytes()); // recursion desired
    q.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0]); // one question, no other records
    for label in name.split('.') {
        ensure!(
            !label.is_empty() && label.len() <= 63,
            "invalid hostname {name:?}"
        );
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&TYPE_SRV.to_be_bytes());
    q.extend_from_slice(&CLASS_IN.to_be_bytes());
    Ok(q)
}

fn exchange(ns: SocketAddr, query: &[u8], id: u16, timeout: Duration) -> Result<Vec<u8>> {
    let local: SocketAddr = if ns.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }.parse()?;
    let socket = UdpSocket::bind(local)?;
    socket.connect(ns)?;
    socket.send(query)?;

    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 4096];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        ensure!(
            !remaining.is_zero(),
            "timed out after {}s",
            timeout.as_secs()
        );
        socket.set_read_timeout(Some(remaining))?;
        let n = match socket.recv(&mut buf) {
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                bail!("timed out after {}s", timeout.as_secs())
            }
            Err(e) => return Err(e.into()),
        };
        // Ignore stray datagrams that aren't the answer to our question.
        if n >= 2 && buf[..2] == id.to_be_bytes() {
            return Ok(buf[..n].to_vec());
        }
    }
}

fn parse_response(msg: &[u8], id: u16) -> Result<Option<(String, u16)>> {
    let mut r = Reader { msg, pos: 0 };
    ensure!(r.u16()? == id, "response id doesn't match the query");
    let flags = r.u16()?;
    ensure!(
        flags & FLAG_RESPONSE != 0,
        "got a query instead of a response"
    );
    match flags & 0x000F {
        0 => {}
        RCODE_NXDOMAIN => return Ok(None),
        rcode => bail!("DNS server returned error code {rcode}"),
    }
    let questions = r.u16()?;
    let answers = r.u16()?;
    r.skip(4)?; // authority + additional counts

    for _ in 0..questions {
        r.name()?;
        r.skip(4)?; // type + class
    }

    // Lowest priority wins; among equals, the heaviest weight.
    let mut best: Option<(u16, Reverse<u16>, String, u16)> = None;
    for _ in 0..answers {
        r.name()?;
        let rtype = r.u16()?;
        r.skip(6)?; // class + ttl
        let rdata_len = r.u16()? as usize;
        let rdata_end = r.pos + rdata_len;
        ensure!(rdata_end <= msg.len(), "DNS response is truncated");
        if rtype == TYPE_SRV {
            let priority = r.u16()?;
            let weight = Reverse(r.u16()?);
            let port = r.u16()?;
            let target = r.name()?;
            if best
                .as_ref()
                .is_none_or(|b| (priority, weight) < (b.0, b.1))
            {
                best = Some((priority, weight, target, port));
            }
        }
        r.pos = rdata_end;
    }

    if best.is_none() && flags & FLAG_TRUNCATED != 0 {
        bail!("DNS response was truncated");
    }
    // A target of "." means the domain explicitly has no Minecraft service.
    Ok(best
        .filter(|b| !b.2.is_empty())
        .map(|(_, _, target, port)| (target, port)))
}

struct Reader<'a> {
    msg: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn u16(&mut self) -> Result<u16> {
        let bytes = self
            .msg
            .get(self.pos..self.pos + 2)
            .context("DNS response is truncated")?;
        self.pos += 2;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn skip(&mut self, n: usize) -> Result<()> {
        ensure!(self.pos + n <= self.msg.len(), "DNS response is truncated");
        self.pos += n;
        Ok(())
    }

    /// Reads a (possibly compressed) domain name and advances past it.
    fn name(&mut self) -> Result<String> {
        let mut labels = Vec::new();
        let mut pos = self.pos;
        let mut jumped = false;
        for _ in 0..128 {
            let len = *self.msg.get(pos).context("DNS name is truncated")? as usize;
            if len == 0 {
                if !jumped {
                    self.pos = pos + 1;
                }
                return Ok(labels.join("."));
            } else if len & 0xC0 == 0xC0 {
                let low = *self.msg.get(pos + 1).context("DNS name is truncated")? as usize;
                if !jumped {
                    self.pos = pos + 2;
                    jumped = true;
                }
                pos = ((len & 0x3F) << 8) | low;
            } else if len & 0xC0 == 0 {
                let label = self
                    .msg
                    .get(pos + 1..pos + 1 + len)
                    .context("DNS name is truncated")?;
                labels.push(String::from_utf8_lossy(label).into_owned());
                pos += 1 + len;
            } else {
                bail!("unsupported DNS label type");
            }
        }
        bail!("DNS name has a compression loop")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a response to `build_query` with the given SRV answers, using a
    /// compression pointer back to the question name like real servers do.
    fn response(id: u16, rcode: u16, answers: &[(u16, u16, u16, &str)]) -> Vec<u8> {
        let mut msg = build_query(id, "_minecraft._tcp.example.com").unwrap();
        msg[2..4].copy_from_slice(&(0x8180 | rcode).to_be_bytes());
        msg[6..8].copy_from_slice(&(answers.len() as u16).to_be_bytes());
        for &(priority, weight, port, target) in answers {
            msg.extend_from_slice(&[0xC0, 12]); // pointer to the question name
            msg.extend_from_slice(&TYPE_SRV.to_be_bytes());
            msg.extend_from_slice(&CLASS_IN.to_be_bytes());
            msg.extend_from_slice(&300u32.to_be_bytes());
            let mut rdata = Vec::new();
            rdata.extend_from_slice(&priority.to_be_bytes());
            rdata.extend_from_slice(&weight.to_be_bytes());
            rdata.extend_from_slice(&port.to_be_bytes());
            for label in target.split('.').filter(|l| !l.is_empty()) {
                rdata.push(label.len() as u8);
                rdata.extend_from_slice(label.as_bytes());
            }
            rdata.push(0);
            msg.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
            msg.extend_from_slice(&rdata);
        }
        msg
    }

    #[test]
    fn picks_lowest_priority_then_highest_weight() {
        let msg = response(
            7,
            0,
            &[
                (20, 100, 1111, "backup.example.com"),
                (10, 5, 2222, "light.example.com"),
                (10, 50, 25600, "mc.example.com"),
            ],
        );
        assert_eq!(
            parse_response(&msg, 7).unwrap(),
            Some(("mc.example.com".into(), 25600))
        );
    }

    #[test]
    fn nxdomain_means_no_record() {
        assert_eq!(
            parse_response(&response(1, RCODE_NXDOMAIN, &[]), 1).unwrap(),
            None
        );
    }

    #[test]
    fn no_answers_means_no_record() {
        assert_eq!(parse_response(&response(1, 0, &[]), 1).unwrap(), None);
    }

    #[test]
    fn dot_target_means_no_record() {
        assert_eq!(
            parse_response(&response(1, 0, &[(0, 0, 25565, ".")]), 1).unwrap(),
            None
        );
    }

    #[test]
    fn rejects_wrong_id_and_server_errors() {
        assert!(parse_response(&response(1, 0, &[]), 2).is_err());
        assert!(parse_response(&response(1, 2, &[]), 1).is_err()); // SERVFAIL
    }

    #[test]
    fn rejects_compression_loops() {
        let mut msg = response(1, 0, &[]);
        msg.truncate(12);
        msg[4..6].copy_from_slice(&1u16.to_be_bytes());
        msg.extend_from_slice(&[0xC0, 12]); // name pointing at itself
        assert!(parse_response(&msg, 1).is_err());
    }
}
