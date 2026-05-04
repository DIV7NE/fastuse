fn main() {
    #[cfg(windows)]
    {
        embed_resource::compile("manifest.rc", embed_resource::NONE);
    }
}
