//! Notice sleep/wake and network changes, so tunnels reconnect at once
//! instead of waiting for ssh's keep-alive timeout or the retry backoff.
//!
//! - Wake: the wall clock jumps ahead of our (paused) timer.
//! - Network: the local address used for outgoing traffic changes, or
//!   the network comes back. Finding it only asks the routing table (a UDP
//!   `connect` sends nothing).

use std::net::{IpAddr, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crate::tunnel::{Kick, Manager};

const TICK: Duration = Duration::from_secs(5);
/// A tick this much later than due means the machine was asleep.
const WAKE_GAP: Duration = Duration::from_secs(20);

/// The source address for traffic to the internet; IPv4 first, since
/// temporary IPv6 addresses rotate on their own.
fn primary_addr() -> Option<IpAddr> {
    let probe = |bind: &str, to: &str| {
        let socket = UdpSocket::bind(bind).ok()?;
        socket.connect(to).ok()?;
        Some(socket.local_addr().ok()?.ip())
    };
    probe("0.0.0.0:0", "8.8.8.8:53").or_else(|| probe("[::]:0", "[2001:4860:4860::8888]:53"))
}

/// Tracks the address across ticks.
#[derive(Default)]
struct Network {
    /// The last address seen while online.
    last: Option<IpAddr>,
    offline: bool,
}

impl Network {
    fn update(&mut self, now: Option<IpAddr>) -> Option<Kick> {
        let Some(addr) = now else {
            self.offline = self.last.is_some();
            return None;
        };
        let kick = match self.last {
            None => None, // first look
            Some(last) if last != addr => Some(Kick::NetworkChanged),
            Some(_) if self.offline => Some(Kick::NetworkBack),
            Some(_) => None,
        };
        self.last = Some(addr);
        self.offline = false;
        kick
    }
}

pub fn spawn(mgr: Arc<Manager>) {
    tauri::async_runtime::spawn(async move {
        let mut network = Network::default();
        network.update(primary_addr());
        let mut last_tick = SystemTime::now();
        loop {
            tokio::time::sleep(TICK).await;
            let now = SystemTime::now();
            let woke = now
                .duration_since(last_tick)
                .is_ok_and(|gap| gap > TICK + WAKE_GAP);
            last_tick = now;
            let changed = network.update(primary_addr());
            if let Some(why) = if woke { Some(Kick::Wake) } else { changed } {
                mgr.kick(why);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_address_changes() {
        let a: IpAddr = "192.168.1.5".parse().unwrap();
        let b: IpAddr = "10.0.0.7".parse().unwrap();
        let mut n = Network::default();
        assert_eq!(n.update(None), None);
        assert_eq!(n.update(Some(a)), None);
        assert_eq!(n.update(Some(a)), None);
        assert_eq!(n.update(Some(b)), Some(Kick::NetworkChanged));
        assert_eq!(n.update(None), None);
        assert_eq!(n.update(None), None);
        assert_eq!(n.update(Some(b)), Some(Kick::NetworkBack));
        assert_eq!(n.update(None), None);
        assert_eq!(n.update(Some(a)), Some(Kick::NetworkChanged));
        assert_eq!(n.update(Some(a)), None);
    }

    #[test]
    fn finds_an_address_without_sending() {
        // Just must not panic; CI machines may have no route.
        let _ = primary_addr();
    }
}
