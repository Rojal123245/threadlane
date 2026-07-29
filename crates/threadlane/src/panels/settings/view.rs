//! Settings modal presentation helpers.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[allow(dead_code)]
pub enum SettingsTab {
    #[default]
    GoogleAntigravity,
    OpenAi,
    WasiExtensions,
    Skills,
    McpServers,
    About,
}

#[allow(dead_code)]
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
