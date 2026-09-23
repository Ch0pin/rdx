fn main() {
    println!("cargo:rerun-if-changed=assets/icons/rdx.ico");
    #[cfg(windows)]
    {
        winresource::WindowsResource::new()
            .set_icon("assets/icons/rdx.ico")
            .set("ProductName", "RDX")
            .set("FileDescription", "RDX APK Decompiler")
            .compile()
            .expect("Could not compile the RDX Windows icon resource");
    }
}
