//! USB board and cable detection, and hotplug monitoring.
//!
//! [`scan`] lists the USB devices present now; [`watch`] runs a background
//! thread on a udev monitor and reports arrivals and departures. Each device
//! is [`identify`]-ed against the board profiles (`data/boards.toml`) first,
//! then against openFPGALoader's cable table (read from `--list-cables`, so it
//! tracks whatever openFPGALoader version is installed).

use std::collections::HashMap;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, LazyLock};
use std::thread::JoinHandle;

use fpgaflow_core::board::{self, Board};
use nix::poll::{poll, PollFd, PollFlags};
use regex::Regex;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UsbDevice {
    pub vid: u16,
    pub pid: u16,
    pub bus: u16,
    pub dev: u16,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub serial: Option<String>,
    /// Stable key for this device instance while it is plugged in.
    pub syspath: PathBuf,
}

impl UsbDevice {
    pub fn id(&self) -> String {
        format!("{:04x}:{:04x}", self.vid, self.pid)
    }

    fn from_udev(d: &udev::Device) -> Option<Self> {
        let attr = |name: &str| {
            d.attribute_value(name)
                .and_then(|v| v.to_str())
                .map(|s| s.trim().to_string())
        };
        let hex = |name: &str| attr(name).and_then(|s| u16::from_str_radix(&s, 16).ok());
        let dec = |name: &str| attr(name).and_then(|s| s.parse().ok());
        Some(Self {
            vid: hex("idVendor")?,
            pid: hex("idProduct")?,
            bus: dec("busnum").unwrap_or(0),
            dev: dec("devnum").unwrap_or(0),
            manufacturer: attr("manufacturer"),
            product: attr("product"),
            serial: attr("serial"),
            syspath: d.syspath().to_path_buf(),
        })
    }
}

/// A cable openFPGALoader knows, from `--list-cables`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Cable {
    pub name: String,
    pub vid: u16,
    pub pid: u16,
}

static CABLE_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(\S+?)\s*0x([0-9a-fA-F]{4}):(?:0x)?([0-9a-fA-F]{4})").unwrap()
});

/// Parse `openFPGALoader --list-cables`. Long names run straight into the
/// ID column (`xilinxPlatformCableUsb_alt0x03fd:000d`), which the pattern allows.
pub fn parse_cable_list(text: &str) -> Vec<Cable> {
    text.lines()
        .filter_map(|l| CABLE_LINE.captures(l))
        .filter_map(|c| {
            Some(Cable {
                name: c[1].to_string(),
                vid: u16::from_str_radix(&c[2], 16).ok()?,
                pid: u16::from_str_radix(&c[3], 16).ok()?,
            })
        })
        .collect()
}

/// Run `openFPGALoader --list-cables` and parse it (empty if it can't run).
pub fn load_cable_list(openfpgaloader: &Path) -> Vec<Cable> {
    std::process::Command::new(openfpgaloader)
        .arg("--list-cables")
        .stdin(std::process::Stdio::null())
        .output()
        .map(|o| parse_cable_list(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Identity {
    /// A board with a profile in `data/boards.toml`.
    Board {
        id: String,
        name: String,
    },
    /// A programming cable openFPGALoader supports, board unknown.
    Cable {
        names: Vec<String>,
    },
    Unknown,
}

pub fn identify(dev: &UsbDevice, cables: &[Cable]) -> Identity {
    if let Some(b) = board::boards()
        .iter()
        .find(|b| b.matches_usb(dev.vid, dev.pid))
    {
        return Identity::Board {
            id: b.id.clone(),
            name: b.name.clone(),
        };
    }
    let names: Vec<String> = cables
        .iter()
        .filter(|c| c.vid == dev.vid && c.pid == dev.pid)
        .map(|c| c.name.clone())
        .collect();
    if names.is_empty() {
        Identity::Unknown
    } else {
        Identity::Cable { names }
    }
}

/// The board profile for an identified device, if any.
pub fn board_of(identity: &Identity) -> Option<&'static Board> {
    match identity {
        Identity::Board { id, .. } => board::board(id),
        _ => None,
    }
}

/// USB devices present now (hubs included; filter with [`identify`]).
pub fn scan() -> std::io::Result<Vec<UsbDevice>> {
    let mut e = udev::Enumerator::new()?;
    e.match_subsystem("usb")?;
    e.match_property("DEVTYPE", "usb_device")?;
    Ok(e.scan_devices()?
        .filter_map(|d| UsbDevice::from_udev(&d))
        .collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hotplug {
    Added(UsbDevice),
    /// Attributes are gone by the time udev reports a removal, so the
    /// watcher reports the device as it was when it arrived (or was scanned).
    Removed(UsbDevice),
}

/// Handle to a running watcher; dropping it stops the thread.
pub struct Watcher {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// How long the watcher blocks in `poll()` before re-checking its stop
/// flag. It bounds shutdown latency only; events are delivered as they arrive.
const POLL_MS: u16 = 250;

/// Watch for USB devices arriving and leaving. `initial` should be the
/// result of a [`scan`] so removals of already-present devices can be named.
pub fn watch(initial: &[UsbDevice], tx: Sender<Hotplug>) -> std::io::Result<Watcher> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let mut known: HashMap<PathBuf, UsbDevice> = initial
        .iter()
        .map(|d| (d.syspath.clone(), d.clone()))
        .collect();
    // libudev handles are not Send, so the monitor is created on the watcher
    // thread itself; setup errors come back over `ready`.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<std::io::Result<()>>();
    let thread = std::thread::Builder::new()
        .name("usb-hotplug".into())
        .spawn(move || {
            let socket = match udev::MonitorBuilder::new()
                .and_then(|b| b.match_subsystem_devtype("usb", "usb_device"))
                .and_then(|b| b.listen())
            {
                Ok(s) => {
                    let _ = ready_tx.send(Ok(()));
                    s
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            while !stop2.load(Ordering::Relaxed) {
                // SAFETY: `socket` lives for the whole loop; the fd is only polled here.
                let fd = unsafe { BorrowedFd::borrow_raw(socket.as_raw_fd()) };
                let mut fds = [PollFd::new(fd, PollFlags::POLLIN)];
                match poll(&mut fds, POLL_MS) {
                    Ok(0) => continue,
                    Ok(_) => {}
                    Err(nix::errno::Errno::EINTR) => continue,
                    Err(_) => break,
                }
                for ev in socket.iter() {
                    let msg = match ev.event_type() {
                        udev::EventType::Add => UsbDevice::from_udev(&ev).map(|d| {
                            known.insert(d.syspath.clone(), d.clone());
                            Hotplug::Added(d)
                        }),
                        udev::EventType::Remove => known.remove(ev.syspath()).map(Hotplug::Removed),
                        _ => None,
                    };
                    if let Some(m) = msg {
                        if tx.send(m).is_err() {
                            return; // receiver gone
                        }
                    }
                }
            }
        })?;
    let watcher = Watcher {
        stop,
        thread: Some(thread),
    };
    match ready_rx.recv() {
        Ok(Ok(())) => Ok(watcher),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(std::io::Error::other("hotplug thread exited during setup")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines as printed by openFPGALoader v1.2.0 on this machine.
    const LIST: &str = "cable name               vid:pid
xilinxPlatformCableUsb   0x03fd:0013
xilinxPlatformCableUsb_alt0x03fd:000d
digilent                 0x0403:6010
ft2232                   0x0403:6010
";

    fn dev(vid: u16, pid: u16) -> UsbDevice {
        UsbDevice {
            vid,
            pid,
            bus: 1,
            dev: 2,
            manufacturer: None,
            product: None,
            serial: None,
            syspath: "/sys/x".into(),
        }
    }

    #[test]
    fn cable_list_parsing() {
        let c = parse_cable_list(LIST);
        assert_eq!(c.len(), 4);
        assert_eq!(
            c[1],
            Cable {
                name: "xilinxPlatformCableUsb_alt".into(),
                vid: 0x03fd,
                pid: 0x000d
            }
        );
    }

    #[test]
    fn identification_prefers_board_profiles() {
        let cables = parse_cable_list(LIST);
        assert!(
            matches!(identify(&dev(0x03fd, 0x000d), &cables), Identity::Board { id, .. } if id == "s3an-starter")
        );
        assert!(matches!(
            identify(&dev(0x03fd, 0x0008), &cables),
            Identity::Board { .. }
        ));
        assert_eq!(
            identify(&dev(0x0403, 0x6010), &cables),
            Identity::Cable {
                names: vec!["digilent".into(), "ft2232".into()]
            }
        );
        assert_eq!(identify(&dev(0x046d, 0xc52e), &cables), Identity::Unknown);
    }

    #[test]
    fn scan_works_on_this_host() {
        // Every Linux machine has at least a root hub.
        assert!(scan().unwrap().iter().any(|d| d.vid == 0x1d6b));
    }
}
