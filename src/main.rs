#![cfg_attr(windows, windows_subsystem = "windows")]
//! Glyphspin: one glyph-rendered 3D object slowly turning as the live desktop wallpaper.
//! This file is the CLI shared by both platforms (--quit, --dump, --send, --offline); everything else is the
//! platform-free core (core/) driven by a platform backend (src/platform/).

mod platform;

use platform::{Msg, Os, Platform};

fn main() {
    glyphspin_core::util::set_app_dir(Os::app_dir);
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--quit") {
        std::process::exit(Os::quit_running());
    }
    if let Some(i) = args.iter().position(|a| a == "--dump") {
        let path = args.get(i + 1).cloned().unwrap_or_else(|| "frame.bmp".into());
        std::process::exit(cli_dump(&path));
    }
    if let Some(code) = Os::test_cli(&args) {
        std::process::exit(code);
    }
    if args.iter().any(|a| a == "--offline") {
        std::process::exit(glyphspin_core::offline::run(&args, Os::build_atlas));
    }
    if let Some(i) = args.iter().position(|a| a == "--send") {
        let cmd = args[i + 1..].join(" ");
        std::process::exit(cli_send(&cmd));
    }
    Os::run(&args);
}

pub fn cli_send(cmd: &str) -> i32 {
    if Os::send(Msg::Cmd, cmd) {
        println!("sent: {cmd}");
        0
    } else {
        println!("not running");
        1
    }
}

/// Ask the running instance to read back its next frame into `path` (BMP) and wait for the stats file next to it.
fn cli_dump(path: &str) -> i32 {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.into());
    let txt = abs.with_extension("txt");
    let _ = std::fs::remove_file(&txt);
    if !Os::send(Msg::Dump, &abs.to_string_lossy()) {
        println!("not running");
        return 1;
    }
    for _ in 0..60 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Ok(s) = std::fs::read_to_string(&txt) {
            if !s.is_empty() {
                println!("{s}");
                return 0;
            }
        }
    }
    println!("no dump written (paused?)");
    1
}
