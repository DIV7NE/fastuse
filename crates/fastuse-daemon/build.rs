fn main() {
    #[cfg(windows)]
    {
        // Embed the application manifest asserting PerMonitorV2 dpiAwareness as a
        // belt-and-braces fallback for the runtime SetProcessDpiAwarenessContext call.
        embed_resource::compile("manifest.rc", embed_resource::NONE);
    }
}
