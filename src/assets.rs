use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "static/"]
pub struct Assets;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_assets_get_index_html() {
        let asset = Assets::get("index.html");
        assert!(asset.is_some());
        let content = asset.unwrap();
        let html = std::str::from_utf8(&content.data).unwrap();
        assert!(html.contains("ttyd-rs"));
        assert!(html.contains("xterm"));
    }

    #[test]
    fn test_assets_get_xterm_js() {
        let asset = Assets::get("vendor/xterm.js");
        assert!(asset.is_some());
    }

    #[test]
    fn test_assets_get_xterm_css() {
        let asset = Assets::get("vendor/xterm.css");
        assert!(asset.is_some());
    }

    #[test]
    fn test_assets_get_fit_addon() {
        let asset = Assets::get("vendor/xterm-addon-fit.js");
        assert!(asset.is_some());
    }

    #[test]
    fn test_assets_get_web_links_addon() {
        let asset = Assets::get("vendor/xterm-addon-web-links.js");
        assert!(asset.is_some());
    }

    #[test]
    fn test_assets_get_nonexistent_returns_none() {
        let asset = Assets::get("nonexistent.txt");
        assert!(asset.is_none());
    }

    #[test]
    fn test_assets_get_nonexistent_deep_path() {
        let asset = Assets::get("some/deep/path/file.js");
        assert!(asset.is_none());
    }

    #[test]
    fn test_assets_index_html_contains_required_elements() {
        let asset = Assets::get("index.html").unwrap();
        let html = std::str::from_utf8(&asset.data).unwrap();
        assert!(html.contains("id=\"terminal\""));
        assert!(html.contains("id=\"login-overlay\""));
        assert!(html.contains("WebSocket"));
    }
}
