//! Single instance and the CLI channel on macOS: a Unix socket in the settings folder. The running instance listens
//! on a thread and hands each message to the main thread; `--send`, `--dump` and `--quit` connect and write one
//! line (`cmd TEXT`, `dump PATH` or `quit`). A connect that succeeds means an instance is running.

use super::super::Msg;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};

fn path() -> std::path::PathBuf {
    glyphspin_core::util::app_dir().join("glyphspin.sock")
}

pub fn running() -> bool {
    UnixStream::connect(path()).is_ok()
}

fn write_line(line: &str) -> bool {
    match UnixStream::connect(path()) {
        Ok(mut s) => s.write_all(line.as_bytes()).is_ok(),
        Err(_) => false,
    }
}

pub fn send(kind: Msg, text: &str) -> bool {
    write_line(&format!("{} {text}", if kind == Msg::Dump { "dump" } else { "cmd" }))
}

/// Ask the running instance to quit and wait (up to 5 s) until its socket stops answering.
pub fn quit() -> i32 {
    if !write_line("quit") {
        println!("not running");
        return 0;
    }
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if !running() {
            println!("quit ok");
            return 0;
        }
    }
    println!("timed out waiting for the instance to quit");
    1
}

/// Listen for messages; `deliver` runs on the listener thread and must hop to the main thread itself.
pub fn serve(deliver: fn(Msg, String, bool)) {
    let p = path();
    let _ = std::fs::remove_file(&p); // stale socket from a crash (a live instance was ruled out by `running`)
    let Ok(listener) = UnixListener::bind(&p) else {
        glyphspin_core::logf!("ipc: can't bind {}", p.display());
        return;
    };
    let _ = std::thread::Builder::new().name("ipc".into()).spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut c) = conn else { continue };
            let mut text = String::new();
            let _ = c.set_read_timeout(Some(std::time::Duration::from_secs(2)));
            if c.read_to_string(&mut text).is_err() {
                continue;
            }
            let text = text.trim().to_string();
            if text == "quit" {
                deliver(Msg::Cmd, String::new(), true);
            } else if let Some(t) = text.strip_prefix("dump ") {
                deliver(Msg::Dump, t.to_string(), false);
            } else if let Some(t) = text.strip_prefix("cmd ") {
                deliver(Msg::Cmd, t.to_string(), false);
            }
        }
    });
}

pub fn remove() {
    let _ = std::fs::remove_file(path());
}
