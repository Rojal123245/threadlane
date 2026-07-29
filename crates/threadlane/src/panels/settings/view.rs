//! Settings modal presentation helpers.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SettingsTab {
    #[default]
    GoogleAntigravity,
    OpenAi,
    WasiExtensions,
    Skills,
    McpServers,
    About,
}

impl SettingsTab {
    pub fn title(&self) -> &'static str {
        match self {
            Self::GoogleAntigravity => "Google Antigravity",
            Self::OpenAi => "OpenAI / ChatGPT",
            Self::WasiExtensions => "WASI Extensions",
            Self::Skills => "Skills",
            Self::McpServers => "MCP Servers",
            Self::About => "About",
        }
    }
}
