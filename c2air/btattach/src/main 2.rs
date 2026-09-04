//! Thin wrapper around [`c2air_btattach::Attach`]: attach, report, then hold the fd forever.
//!
//! Exists for bring-up and for proving the attach standalone. In production OCBM owns Bluetooth and
//! should link the library instead, because the attach lives exactly as long as the fd — see the
//! library's module docs.
//!
//!   c2air-btattach [DEV] [BAUD]
//!
//! With no arguments it uses `LY_BT_UART` / `LY_BT_BDRATE` from the board's own `/etc/profile`.

use std::os::unix::io::AsRawFd;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.iter().any(|x| x == "-h" || x == "--help") {
        eprintln!("usage: c2air-btattach [DEV] [BAUD]   (defaults from LY_BT_UART / LY_BT_BDRATE)");
        std::process::exit(0);
    }
    let dev = a.get(1).cloned().unwrap_or_else(c2air_btattach::board_uart);
    let baud = match a.get(2) {
        Some(s) => match s.parse() {
            Ok(v) => v,
            Err(_) => {
                eprintln!("[btattach] bad baud {s:?}");
                std::process::exit(2);
            }
        },
        None => c2air_btattach::board_baud(),
    };

    let att = match c2air_btattach::Attach::open(&dev, baud) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[btattach] FAILED on {dev} @{baud}: {e}");
            std::process::exit(1);
        }
    };
    println!("[btattach] attached as hci0 — {} @{} (fd {})", att.dev, att.baud, att.as_raw_fd());
    println!("[btattach] verify:  ls /sys/class/bluetooth/  and  cat /proc/tty/driver/* for tx/rx counters");
    println!("[btattach] holding the fd; the attach ends when this process does");

    // Park. Not a spin: pause() sleeps until a signal arrives, and the default disposition of
    // SIGTERM/SIGINT terminates us, which drops the Attach and detaches. Looping because pause()
    // also returns on signals we might inherit as handled/ignored.
    loop {
        unsafe { libc::pause() };
    }
}
