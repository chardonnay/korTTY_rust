use crate::model::gui_theme::GuiTheme;
use crate::persistence::xml_repository;

const GUI_THEMES_FILE: &str = "gui-themes.json";

fn builtin_gui_themes() -> Vec<GuiTheme> {
    vec![
        GuiTheme {
            id: "builtin-catppuccin-mocha".into(),
            name: "Catppuccin Mocha".into(),
            bg: "#1e1e2e".into(),
            surface: "#252536".into(),
            panel: "#2a2a3c".into(),
            border: "#3a3a4c".into(),
            text: "#cdd6f4".into(),
            text_dim: "#6c7086".into(),
            accent: "#89b4fa".into(),
            accent_hover: "#74a8fc".into(),
            success: "#a6e3a1".into(),
            warning: "#f9e2af".into(),
            error: "#f38ba8".into(),
            terminal: "#11111b".into(),
        },
        GuiTheme {
            id: "builtin-dracula".into(),
            name: "Dracula".into(),
            bg: "#282a36".into(),
            surface: "#2d2f3d".into(),
            panel: "#343746".into(),
            border: "#44475a".into(),
            text: "#f8f8f2".into(),
            text_dim: "#6272a4".into(),
            accent: "#bd93f9".into(),
            accent_hover: "#a87ced".into(),
            success: "#50fa7b".into(),
            warning: "#f1fa8c".into(),
            error: "#ff5555".into(),
            terminal: "#1e1f29".into(),
        },
        GuiTheme {
            id: "builtin-nord".into(),
            name: "Nord".into(),
            bg: "#2e3440".into(),
            surface: "#343a48".into(),
            panel: "#3b4252".into(),
            border: "#434c5e".into(),
            text: "#eceff4".into(),
            text_dim: "#7b88a1".into(),
            accent: "#88c0d0".into(),
            accent_hover: "#7ab5c5".into(),
            success: "#a3be8c".into(),
            warning: "#ebcb8b".into(),
            error: "#bf616a".into(),
            terminal: "#242933".into(),
        },
        GuiTheme {
            id: "builtin-gruvbox-dark".into(),
            name: "Gruvbox Dark".into(),
            bg: "#282828".into(),
            surface: "#2e2e2e".into(),
            panel: "#3c3836".into(),
            border: "#504945".into(),
            text: "#ebdbb2".into(),
            text_dim: "#928374".into(),
            accent: "#fabd2f".into(),
            accent_hover: "#e5ab27".into(),
            success: "#b8bb26".into(),
            warning: "#fe8019".into(),
            error: "#fb4934".into(),
            terminal: "#1d2021".into(),
        },
        GuiTheme {
            id: "builtin-one-dark".into(),
            name: "One Dark".into(),
            bg: "#282c34".into(),
            surface: "#2c313a".into(),
            panel: "#333842".into(),
            border: "#3e4451".into(),
            text: "#abb2bf".into(),
            text_dim: "#636d83".into(),
            accent: "#61afef".into(),
            accent_hover: "#519fdf".into(),
            success: "#98c379".into(),
            warning: "#e5c07b".into(),
            error: "#e06c75".into(),
            terminal: "#1e2127".into(),
        },
        GuiTheme {
            id: "builtin-tokyo-night".into(),
            name: "Tokyo Night".into(),
            bg: "#1a1b26".into(),
            surface: "#1f2030".into(),
            panel: "#24283b".into(),
            border: "#33467c".into(),
            text: "#c0caf5".into(),
            text_dim: "#565f89".into(),
            accent: "#7aa2f7".into(),
            accent_hover: "#6a92e7".into(),
            success: "#9ece6a".into(),
            warning: "#e0af68".into(),
            error: "#f7768e".into(),
            terminal: "#13141c".into(),
        },
        GuiTheme {
            id: "builtin-monokai".into(),
            name: "Monokai Pro".into(),
            bg: "#2d2a2e".into(),
            surface: "#332f33".into(),
            panel: "#3a363a".into(),
            border: "#4a464a".into(),
            text: "#fcfcfa".into(),
            text_dim: "#8b888f".into(),
            accent: "#ffd866".into(),
            accent_hover: "#ecc85c".into(),
            success: "#a9dc76".into(),
            warning: "#fc9867".into(),
            error: "#ff6188".into(),
            terminal: "#221f22".into(),
        },
        GuiTheme {
            id: "builtin-github-dark".into(),
            name: "GitHub Dark".into(),
            bg: "#0d1117".into(),
            surface: "#161b22".into(),
            panel: "#1c2128".into(),
            border: "#30363d".into(),
            text: "#c9d1d9".into(),
            text_dim: "#7d8590".into(),
            accent: "#58a6ff".into(),
            accent_hover: "#4896ef".into(),
            success: "#3fb950".into(),
            warning: "#d29922".into(),
            error: "#f85149".into(),
            terminal: "#010409".into(),
        },
        GuiTheme {
            id: "builtin-solarized-dark".into(),
            name: "Solarized Dark".into(),
            bg: "#002b36".into(),
            surface: "#003542".into(),
            panel: "#073642".into(),
            border: "#1a4a55".into(),
            text: "#839496".into(),
            text_dim: "#586e75".into(),
            accent: "#268bd2".into(),
            accent_hover: "#1a7dc2".into(),
            success: "#859900".into(),
            warning: "#b58900".into(),
            error: "#dc322f".into(),
            terminal: "#001e26".into(),
        },
        GuiTheme {
            id: "builtin-everforest".into(),
            name: "Everforest Dark".into(),
            bg: "#2d353b".into(),
            surface: "#323c41".into(),
            panel: "#374247".into(),
            border: "#4a555b".into(),
            text: "#d3c6aa".into(),
            text_dim: "#7a8478".into(),
            accent: "#a7c080".into(),
            accent_hover: "#97b070".into(),
            success: "#83c092".into(),
            warning: "#dbbc7f".into(),
            error: "#e67e80".into(),
            terminal: "#252d32".into(),
        },
        GuiTheme {
            id: "builtin-material-dark".into(),
            name: "Material Dark".into(),
            bg: "#263238".into(),
            surface: "#2c393f".into(),
            panel: "#344046".into(),
            border: "#425762".into(),
            text: "#eeffff".into(),
            text_dim: "#607d8b".into(),
            accent: "#82aaff".into(),
            accent_hover: "#729aef".into(),
            success: "#c3e88d".into(),
            warning: "#ffcb6b".into(),
            error: "#ff5370".into(),
            terminal: "#1a2327".into(),
        },
        GuiTheme {
            id: "builtin-ayu-dark".into(),
            name: "Ayu Dark".into(),
            bg: "#0d1017".into(),
            surface: "#131721".into(),
            panel: "#1a1f29".into(),
            border: "#2a3040".into(),
            text: "#bfbdb6".into(),
            text_dim: "#636a76".into(),
            accent: "#e6b450".into(),
            accent_hover: "#d6a440".into(),
            success: "#91b362".into(),
            warning: "#f9af4f".into(),
            error: "#ea6c73".into(),
            terminal: "#070a0f".into(),
        },
        GuiTheme {
            id: "builtin-rose-pine".into(),
            name: "Rosé Pine".into(),
            bg: "#191724".into(),
            surface: "#1f1d2e".into(),
            panel: "#26233a".into(),
            border: "#393552".into(),
            text: "#e0def4".into(),
            text_dim: "#6e6a86".into(),
            accent: "#c4a7e7".into(),
            accent_hover: "#b497d7".into(),
            success: "#9ccfd8".into(),
            warning: "#f6c177".into(),
            error: "#eb6f92".into(),
            terminal: "#12101e".into(),
        },
        GuiTheme {
            id: "builtin-light-default".into(),
            name: "KorTTY Light".into(),
            bg: "#f5f5f5".into(),
            surface: "#ffffff".into(),
            panel: "#eaeaea".into(),
            border: "#d4d4d4".into(),
            text: "#1e1e1e".into(),
            text_dim: "#757575".into(),
            accent: "#0078d4".into(),
            accent_hover: "#006bc4".into(),
            success: "#16a34a".into(),
            warning: "#ca8a04".into(),
            error: "#dc2626".into(),
            terminal: "#1e1e1e".into(),
        },
        GuiTheme {
            id: "builtin-solarized-light".into(),
            name: "Solarized Light".into(),
            bg: "#fdf6e3".into(),
            surface: "#eee8d5".into(),
            panel: "#e8e2cf".into(),
            border: "#ccc5b3".into(),
            text: "#586e75".into(),
            text_dim: "#93a1a1".into(),
            accent: "#268bd2".into(),
            accent_hover: "#1a7dc2".into(),
            success: "#859900".into(),
            warning: "#b58900".into(),
            error: "#dc322f".into(),
            terminal: "#002b36".into(),
        },
    ]
}

fn load_user_gui_themes() -> Vec<GuiTheme> {
    xml_repository::load_json::<Vec<GuiTheme>>(GUI_THEMES_FILE)
        .ok()
        .flatten()
        .unwrap_or_default()
}

fn save_user_gui_themes(themes: &[GuiTheme]) -> Result<(), String> {
    xml_repository::save_json(GUI_THEMES_FILE, themes).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_gui_themes() -> Result<Vec<GuiTheme>, String> {
    let builtins = builtin_gui_themes();
    let user = load_user_gui_themes();
    let mut all = builtins;
    all.extend(user);
    Ok(all)
}

#[tauri::command]
pub async fn save_gui_theme(theme: GuiTheme) -> Result<(), String> {
    let mut user = load_user_gui_themes();
    if let Some(pos) = user.iter().position(|t| t.id == theme.id) {
        user[pos] = theme;
    } else {
        user.push(theme);
    }
    save_user_gui_themes(&user)
}

#[tauri::command]
pub async fn delete_gui_theme(id: String) -> Result<(), String> {
    if id.starts_with("builtin-") {
        return Err("Cannot delete built-in themes".into());
    }
    let mut user = load_user_gui_themes();
    user.retain(|t| t.id != id);
    save_user_gui_themes(&user)
}

#[tauri::command]
pub async fn get_active_gui_theme_id() -> Result<String, String> {
    let id: Option<String> =
        xml_repository::load_json("active-gui-theme.json").map_err(|e| e.to_string())?;
    Ok(id.unwrap_or_else(|| "builtin-catppuccin-mocha".into()))
}

#[tauri::command]
pub async fn set_active_gui_theme_id(id: String) -> Result<(), String> {
    xml_repository::save_json("active-gui-theme.json", &id).map_err(|e| e.to_string())
}
