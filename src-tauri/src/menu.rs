//! Native menu bar + the About window.
//!
//! macOS: the menu lives in the system menu bar (app menu → About SW3 Images).
//! Windows: it is attached to the main window only, with About under Help.

use tauri::menu::{Menu, MenuItemBuilder, SubmenuBuilder};
use tauri::{AppHandle, Emitter, Manager, Runtime, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;

/// External pages the app may open. The webview can only pick one of these
/// by name — it can never make the app open an arbitrary URL.
pub fn link_url(name: &str) -> Option<&'static str> {
    match name {
        "sustainablewww" => Some("https://sustainablewww.se"),
        "michael" => Some("https://michaelandersen.net"),
        "github" => Some("https://github.com/Sustainable-WWW/SW3-Images-Desktop"),
        "releases" => Some("https://github.com/Sustainable-WWW/SW3-Images-Desktop/releases"),
        _ => None,
    }
}

pub fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let about = MenuItemBuilder::with_id("about", "About SW3 Images").build(app)?;
    let check = MenuItemBuilder::with_id("check_updates", "Check for Updates…").build(app)?;
    let add = MenuItemBuilder::with_id("add_images", "Add Images…").accelerator("CmdOrCtrl+O").build(app)?;
    let folder = MenuItemBuilder::with_id("open_folder", "Open Output Folder")
        .accelerator("CmdOrCtrl+Shift+O")
        .build(app)?;
    let website = MenuItemBuilder::with_id("link:sustainablewww", "Sustainable WWW Website").build(app)?;
    let source = MenuItemBuilder::with_id("link:github", "SW3 Images on GitHub").build(app)?;

    let file = SubmenuBuilder::new(app, "File").item(&add).item(&folder).separator().close_window();
    let edit = SubmenuBuilder::new(app, "Edit").undo().redo().separator().cut().copy().paste().select_all().build()?;

    #[cfg(target_os = "macos")]
    {
        let app_menu = SubmenuBuilder::new(app, "SW3 Images")
            .item(&about)
            .item(&check)
            .separator()
            .services()
            .separator()
            .hide()
            .hide_others()
            .show_all()
            .separator()
            .quit()
            .build()?;
        let window = SubmenuBuilder::new(app, "Window").minimize().maximize().fullscreen().separator().close_window().build()?;
        let help = SubmenuBuilder::new(app, "Help").item(&website).item(&source).build()?;
        Menu::with_items(app, &[&app_menu, &file.build()?, &edit, &window, &help])
    }
    #[cfg(not(target_os = "macos"))]
    {
        let file = file.separator().quit().build()?;
        let help = SubmenuBuilder::new(app, "Help")
            .item(&website)
            .item(&source)
            .separator()
            .item(&check)
            .separator()
            .item(&about)
            .build()?;
        Menu::with_items(app, &[&file, &edit, &help])
    }
}

/// Install the menu: system-wide on macOS, main window only elsewhere (so
/// the About window doesn't grow a menu bar on Windows).
pub fn install(app: &AppHandle) -> tauri::Result<()> {
    let menu = build(app)?;
    #[cfg(target_os = "macos")]
    app.set_menu(menu)?;
    #[cfg(not(target_os = "macos"))]
    if let Some(main) = app.get_webview_window("main") {
        main.set_menu(menu)?;
    }
    app.on_menu_event(|app, event| {
        let id = event.id().as_ref();
        match id {
            "about" => {
                let _ = open_about(app);
            }
            "check_updates" | "add_images" => {
                let _ = app.emit_to("main", "menu", id);
            }
            "open_folder" => {
                if let Ok(dir) = crate::settings::output_dir(app) {
                    let _ = app.opener().open_path(dir.to_string_lossy(), None::<&str>);
                }
            }
            _ => {
                if let Some(url) = id.strip_prefix("link:").and_then(link_url) {
                    let _ = app.opener().open_url(url, None::<&str>);
                }
            }
        }
    });
    Ok(())
}

pub fn open_about(app: &AppHandle) -> tauri::Result<()> {
    if let Some(w) = app.get_webview_window("about") {
        w.show()?;
        return w.set_focus();
    }
    let builder = WebviewWindowBuilder::new(app, "about", WebviewUrl::App("about.html".into()))
        .title("About SW3 Images")
        .inner_size(460.0, 730.0)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .center();
    #[cfg(target_os = "macos")]
    let builder = builder.title_bar_style(tauri::TitleBarStyle::Overlay).hidden_title(true);
    builder.build()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::link_url;

    #[test]
    fn only_allowlisted_links_open() {
        assert_eq!(link_url("sustainablewww"), Some("https://sustainablewww.se"));
        assert!(link_url("github").unwrap().starts_with("https://github.com/Sustainable-WWW/"));
        for bad in ["", "https://evil.example", "file:///etc/passwd", "javascript:alert(1)", "GITHUB"] {
            assert_eq!(link_url(bad), None, "{bad}");
        }
        for name in ["sustainablewww", "michael", "github", "releases"] {
            assert!(link_url(name).unwrap().starts_with("https://"));
        }
    }
}
