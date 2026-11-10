//! xray's stats API without running xray: a minimal gRPC client (HTTP/2
//! with prior knowledge, one unary call per stream) on its socket file.
//! `xray api statsquery` forked this process and started Go every second
//! the window was open (the list stuttered with it) and every three seconds
//! in the background (the battery went with it).

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};

const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

const DATA: u8 = 0x0;
const HEADERS: u8 = 0x1;
const RST_STREAM: u8 = 0x3;
const SETTINGS: u8 = 0x4;
const PING: u8 = 0x6;
const GOAWAY: u8 = 0x7;

const END_STREAM: u8 = 0x1;
const ACK: u8 = 0x1;
const END_HEADERS: u8 = 0x4;
const PADDED: u8 = 0x8;

const GET_STATS: &str = "/xray.app.stats.command.StatsService/GetStats";

/// The counters named, in order. One xray has not made yet (nothing went
/// through) reads 0.
pub fn get_stats(socket: &Path, names: &[&str]) -> Result<Vec<u64>> {
    let mut stream = UnixStream::connect(socket).with_context(|| format!("xray api {}", socket.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut out = PREFACE.to_vec();
    frame(&mut out, SETTINGS, 0, 0, &[]);
    for (i, name) in names.iter().enumerate() {
        let id = stream_id(i);
        frame(&mut out, HEADERS, END_HEADERS, id, &request_headers(GET_STATS));
        frame(&mut out, DATA, END_STREAM, id, &grpc_message(&string_field(1, name)));
    }
    stream.write_all(&out)?;

    let mut bodies = vec![Vec::new(); names.len()];
    let mut open = names.len();
    while open > 0 {
        let (kind, flags, id, payload) = read_frame(&mut stream)?;
        match kind {
            SETTINGS if flags & ACK == 0 => {
                let mut ack = Vec::new();
                frame(&mut ack, SETTINGS, ACK, 0, &[]);
                stream.write_all(&ack)?;
            }
            PING if flags & ACK == 0 => {
                let mut pong = Vec::new();
                frame(&mut pong, PING, ACK, 0, &payload);
                stream.write_all(&pong)?;
            }
            GOAWAY => bail!("xray api: the connection was closed"),
            RST_STREAM => bail!("xray api: the call was reset"),
            DATA | HEADERS => {
                let Some(i) = (0..names.len()).find(|&i| stream_id(i) == id) else { continue };
                if kind == DATA {
                    bodies[i].extend_from_slice(unpad(flags, &payload)?);
                }
                if flags & END_STREAM != 0 {
                    open -= 1;
                }
            }
            _ => {}
        }
    }
    // A call without a body ended with an error status ("not found").
    Ok(bodies.iter().map(|b| stat_value(b).unwrap_or(0)).collect())
}

fn stream_id(call: usize) -> u32 {
    1 + 2 * call as u32
}

fn frame(out: &mut Vec<u8>, kind: u8, flags: u8, id: u32, payload: &[u8]) {
    let len = payload.len() as u32;
    out.extend_from_slice(&len.to_be_bytes()[1..]);
    out.extend_from_slice(&[kind, flags]);
    out.extend_from_slice(&(id & 0x7fff_ffff).to_be_bytes());
    out.extend_from_slice(payload);
}

fn read_frame(stream: &mut impl Read) -> Result<(u8, u8, u32, Vec<u8>)> {
    let mut head = [0u8; 9];
    stream.read_exact(&mut head).context("xray api: no answer")?;
    let len = u32::from_be_bytes([0, head[0], head[1], head[2]]) as usize;
    if len > 1 << 20 {
        bail!("xray api: a frame of {len} bytes");
    }
    let mut payload = vec![0; len];
    stream.read_exact(&mut payload)?;
    let id = u32::from_be_bytes([head[5], head[6], head[7], head[8]]) & 0x7fff_ffff;
    Ok((head[3], head[4], id, payload))
}

fn unpad(flags: u8, payload: &[u8]) -> Result<&[u8]> {
    if flags & PADDED == 0 {
        return Ok(payload);
    }
    let pad = usize::from(*payload.first().context("xray api: bad padding")?);
    payload.get(1..payload.len().saturating_sub(pad)).context("xray api: bad padding")
}

/// HPACK, no table and no Huffman: :method POST, :scheme http, :path,
/// :authority, content-type and te.
fn request_headers(path: &str) -> Vec<u8> {
    let mut h = vec![0x83, 0x86];
    // Literal without indexing, the name from the static table (4-bit prefix).
    let mut literal = |index: usize, value: &str| {
        hpack_int(&mut h, 4, 0x00, index);
        hpack_string(&mut h, value);
    };
    literal(4, path);
    literal(1, "localhost");
    literal(31, "application/grpc");
    h.push(0x00);
    hpack_string(&mut h, "te");
    hpack_string(&mut h, "trailers");
    h
}

fn hpack_int(out: &mut Vec<u8>, prefix_bits: u32, flags: u8, mut value: usize) {
    let max = (1usize << prefix_bits) - 1;
    if value < max {
        out.push(flags | value as u8);
        return;
    }
    out.push(flags | max as u8);
    value -= max;
    while value >= 128 {
        out.push((value % 128) as u8 | 0x80);
        value /= 128;
    }
    out.push(value as u8);
}

fn hpack_string(out: &mut Vec<u8>, s: &str) {
    hpack_int(out, 7, 0x00, s.len());
    out.extend_from_slice(s.as_bytes());
}

/// A gRPC message: not compressed, its length, the protobuf.
fn grpc_message(message: &[u8]) -> Vec<u8> {
    let mut out = vec![0];
    out.extend_from_slice(&(message.len() as u32).to_be_bytes());
    out.extend_from_slice(message);
    out
}

fn string_field(field: u64, value: &str) -> Vec<u8> {
    let mut out = Vec::new();
    varint(&mut out, field << 3 | 2);
    varint(&mut out, value.len() as u64);
    out.extend_from_slice(value.as_bytes());
    out
}

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn read_varint(buf: &[u8], at: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *buf.get(*at)?;
        *at += 1;
        v |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

/// The protobuf fields of `buf`: (number, varint value or bytes).
fn fields(buf: &[u8]) -> Option<Vec<(u64, Result<u64, &[u8]>)>> {
    let mut at = 0;
    let mut out = Vec::new();
    while at < buf.len() {
        let tag = read_varint(buf, &mut at)?;
        match tag & 7 {
            0 => out.push((tag >> 3, Ok(read_varint(buf, &mut at)?))),
            2 => {
                let len = usize::try_from(read_varint(buf, &mut at)?).ok()?;
                let bytes = buf.get(at..at.checked_add(len)?)?;
                at += len;
                out.push((tag >> 3, Err(bytes)));
            }
            1 => at += 8,
            5 => at += 4,
            _ => return None,
        }
    }
    Some(out)
}

/// GetStatsResponse { Stat stat = 1 { string name = 1; int64 value = 2 } }
/// behind the 5-byte gRPC prefix.
fn stat_value(body: &[u8]) -> Option<u64> {
    let message = body.get(5..)?;
    let stat = fields(message)?.into_iter().find_map(|(n, v)| (n == 1).then_some(v.err()).flatten())?;
    Some(fields(stat)?.into_iter().find_map(|(n, v)| (n == 2).then_some(v.ok()).flatten()).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hpack_integers_as_in_the_rfc() {
        let mut out = Vec::new();
        hpack_int(&mut out, 5, 0, 10);
        assert_eq!(out, [0x0a]);
        out.clear();
        hpack_int(&mut out, 5, 0, 1337);
        assert_eq!(out, [0x1f, 0x9a, 0x0a]);
        out.clear();
        hpack_int(&mut out, 4, 0, 31);
        assert_eq!(out, [0x0f, 0x10]);
    }

    fn stat_response(name: &str, value: u64) -> Vec<u8> {
        let mut stat = string_field(1, name);
        varint(&mut stat, 2 << 3);
        varint(&mut stat, value);
        let mut message = Vec::new();
        varint(&mut message, 1 << 3 | 2);
        varint(&mut message, stat.len() as u64);
        message.extend_from_slice(&stat);
        grpc_message(&message)
    }

    #[test]
    fn calls_over_http2_and_reads_the_counters() {
        let dir = std::env::temp_dir().join(format!("duoray-grpc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("api.sock");
        let _ = std::fs::remove_file(&socket);
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut preface = [0u8; 24];
            s.read_exact(&mut preface).unwrap();
            assert_eq!(&preface, PREFACE);
            // Both calls in: each stream's request ends with its DATA.
            let mut ended = Vec::new();
            let mut asked = Vec::new();
            while ended.len() < 2 {
                let (kind, flags, id, payload) = read_frame(&mut s).unwrap();
                if kind == DATA {
                    asked.push(String::from_utf8_lossy(&payload[7..]).into_owned());
                }
                if flags & END_STREAM != 0 {
                    ended.push(id);
                }
            }
            assert_eq!(ended, [1, 3]);
            let mut out = Vec::new();
            frame(&mut out, SETTINGS, 0, 0, &[0, 3, 0, 0, 0, 100]);
            frame(&mut out, HEADERS, END_HEADERS, 1, &[0x88]);
            frame(&mut out, DATA, 0, 1, &stat_response("inbound>>>socks>>>traffic>>>uplink", 1234));
            frame(&mut out, HEADERS, END_HEADERS | END_STREAM, 1, &[0x00]);
            // Not made yet: trailers only.
            frame(&mut out, HEADERS, END_HEADERS | END_STREAM, 3, &[0x88]);
            s.write_all(&out).unwrap();
            // The client acknowledges the settings.
            let (kind, flags, ..) = read_frame(&mut s).unwrap();
            assert_eq!((kind, flags), (SETTINGS, ACK));
            asked
        });
        let names = ["inbound>>>socks>>>traffic>>>uplink", "inbound>>>socks>>>traffic>>>downlink"];
        assert_eq!(get_stats(&socket, &names).unwrap(), [1234, 0]);
        assert_eq!(server.join().unwrap(), names);
    }
}
