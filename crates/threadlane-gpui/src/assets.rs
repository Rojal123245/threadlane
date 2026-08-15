use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};
use gpui_component_assets::Assets as ComponentAssets;

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let bytes: Option<&'static [u8]> = match path {
            "icons/providers/openai.svg" => {
                Some(include_bytes!("../assets/icons/providers/openai.svg"))
            }
            "icons/providers/google.svg" => {
                Some(include_bytes!("../assets/icons/providers/google.svg"))
            }
            "icons/providers/opencode.svg" => {
                Some(include_bytes!("../assets/icons/providers/opencode.svg"))
            }
            "icons/providers/acp.svg" => Some(include_bytes!("../assets/icons/providers/acp.svg")),
            _ => None,
        };

        match bytes {
            Some(bytes) => Ok(Some(Cow::Borrowed(bytes))),
            None => ComponentAssets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut assets = ComponentAssets.list(path)?;
        assets.extend(
            [
                "icons/providers/openai.svg",
                "icons/providers/google.svg",
                "icons/providers/opencode.svg",
                "icons/providers/acp.svg",
            ]
            .into_iter()
            .filter(|asset| asset.starts_with(path))
            .map(SharedString::from),
        );
        Ok(assets)
    }
}
