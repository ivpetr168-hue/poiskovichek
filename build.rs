fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "windows" {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/poiskovichek.ico");
        res.set("ProductName", "ПоисКовичеК");
        res.set("FileDescription", "Быстрый поиск по документам");
        res.set("OriginalFilename", "Poiskovichek.exe");
        if let Ok(version) = std::env::var("CARGO_PKG_VERSION") {
            res.set("ProductVersion", &version);
            res.set("FileVersion", &version);
        }
        res.compile().unwrap();
    }
}
