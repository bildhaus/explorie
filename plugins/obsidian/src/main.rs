fn main() -> std::io::Result<()> {
    explorie_plugin_protocol::run_stdio(explorie_plugin_obsidian::ObsidianPlugin::default())
}
