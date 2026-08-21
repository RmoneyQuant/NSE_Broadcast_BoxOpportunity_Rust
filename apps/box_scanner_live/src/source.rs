use std::fs;
use std::io;
use std::net::{Ipv4Addr, UdpSocket};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Replays hex-encoded packets from a fixture file, one per line, pacing
/// them out on a delay so they arrive over time -- closer to what a live
/// feed thread hands off than dumping the whole file at once. Loops the
/// fixture indefinitely rather than stopping after one pass: a live feed
/// never runs dry, and `box_scanner_live`'s "Apply Filter" only gets
/// checked when a packet arrives (see `main.rs`'s background loop) -- a
/// one-shot replay would drain the channel, silently end that loop, and
/// leave every later filter change permanently inert. Looping means a
/// filter change made after the first pass still gets fresh matching
/// ticks within one cycle to populate the newly selected pairs.
pub fn spawn_replay(tx: Sender<Vec<u8>>, fixture: PathBuf) -> JoinHandle<()> {
    thread::spawn(move || {
        let contents = fs::read_to_string(&fixture)
            .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", fixture.display()));
        let lines: Vec<&str> = contents.lines().map(str::trim).filter(|l| !l.is_empty()).collect();

        let mut cycle = 0u64;
        loop {
            cycle += 1;
            if cycle > 1 {
                println!("replay: looping fixture again (cycle {cycle})");
            }
            for line in &lines {
                match hex_to_bytes(line) {
                    Ok(bytes) => {
                        if tx.send(bytes).is_err() {
                            return; // receiver side has shut down
                        }
                    }
                    Err(e) => eprintln!("skipping malformed fixture line: {e}"),
                }

                thread::sleep(Duration::from_millis(200));
            }
        }
    })
}

/// Joins the NSE F&O multicast group and forwards each raw UDP datagram
/// as-is. Mirrors `create_multicast_socket()` in `nse_fo.py`.
pub fn spawn_live(
    tx: Sender<Vec<u8>>,
    mcast_grp: String,
    mcast_port: u16,
    local_if: String,
) -> io::Result<JoinHandle<()>> {
    let grp: Ipv4Addr = mcast_grp
        .parse()
        .unwrap_or_else(|_| panic!("NSE_FO_MCAST_GRP is not a valid IPv4 address: {mcast_grp}"));
    let iface: Ipv4Addr = local_if
        .parse()
        .unwrap_or_else(|_| panic!("NSE_FO_LOCAL_IF is not a valid IPv4 address: {local_if}"));

    // std::net::UdpSocket::bind binds and creates in one step, with no way
    // to set socket options first. SO_REUSEADDR has to be set *before*
    // bind() -- matching nse_fo.py's create_multicast_socket() -- or bind()
    // fails outright (os error 10048) the moment anything else already
    // holds the port, e.g. nse_fo.py itself already running against the
    // same feed.
    let raw = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None)?;
    raw.set_reuse_address(true)?;
    let addr: std::net::SocketAddr = ([0, 0, 0, 0], mcast_port).into();
    raw.bind(&addr.into())?;
    let socket: UdpSocket = raw.into();
    socket.join_multicast_v4(&grp, &iface)?;

    Ok(thread::spawn(move || {
        let mut buf = [0u8; 65535];
        loop {
            match socket.recv_from(&mut buf) {
                Ok((n, _addr)) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("multicast recv error: {e}");
                    break;
                }
            }
        }
    }))
}

fn hex_to_bytes(hex: &str) -> Result<Vec<u8>, String> {
    if !hex.len().is_multiple_of(2) {
        return Err(format!("odd-length hex string ({} chars)", hex.len()));
    }

    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_even_length_hex() {
        assert_eq!(hex_to_bytes("4e5345").unwrap(), vec![0x4e, 0x53, 0x45]);
    }

    #[test]
    fn rejects_odd_length_hex() {
        assert!(hex_to_bytes("4e534").is_err());
    }
}
