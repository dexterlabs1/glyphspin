// Embeds the icon and version info into glyphspin.exe (needs rc.exe from the Windows SDK, present on MSVC setups
// and on GitHub's windows-latest runners).
fn main() {
    println!("cargo:rerun-if-changed=assets/glyphspin.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon_with_id("assets/glyphspin.ico", "1");
        res.set("ProductName", "Glyphspin");
        res.set("FileDescription", "Glyphspin - a 3D glyph object as your live wallpaper");
        res.set("LegalCopyright", "Copyright (c) 2026 Dexter Brewer. MIT license.");
        if let Err(e) = res.compile() {
            println!("cargo:warning=icon not embedded: {e}");
        }
    }
}
