//! Windows version information and icon for the exe (product name, version,
//! publisher, assets/overhead.ico). gpui already embeds the application
//! manifest, so only these are added here.

fn main() {
    println!("cargo:rerun-if-env-changed=OVERHEAD_BUILD");
    println!("cargo:rerun-if-changed=assets/overhead.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let build: u64 = std::env::var("OVERHEAD_BUILD").ok().and_then(|b| b.parse().ok()).unwrap_or(0);
    let (maj, min, pat) = (
        env!("CARGO_PKG_VERSION_MAJOR").parse::<u64>().unwrap(),
        env!("CARGO_PKG_VERSION_MINOR").parse::<u64>().unwrap(),
        env!("CARGO_PKG_VERSION_PATCH").parse::<u64>().unwrap(),
    );
    let version = (maj << 48) | (min << 32) | (pat << 16) | build;
    let text = format!("{maj}.{min}.{pat}.{build}");
    let mut r = winresource::WindowsResource::new();
    let ico = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets/overhead.ico");
    r.set_icon(&ico.to_string_lossy());
    r.set("ProductName", "Overhead")
        .set("FileDescription", "Overhead - find where a recording was made from the aircraft heard in it")
        .set("CompanyName", "Piotr Kaczorowski")
        .set("LegalCopyright", "Copyright 2026 Piotr Kaczorowski")
        .set("OriginalFilename", "overhead.exe")
        .set("InternalName", "overhead")
        .set("FileVersion", &text)
        .set("ProductVersion", &text)
        .set_version_info(winresource::VersionInfo::FILEVERSION, version)
        .set_version_info(winresource::VersionInfo::PRODUCTVERSION, version);
    if let Err(e) = r.compile() {
        // Version info is cosmetic; never fail the build over it.
        println!("cargo:warning=no Windows version info: {e}");
    }
}
