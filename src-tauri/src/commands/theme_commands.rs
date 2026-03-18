use crate::model::theme::Theme;
use crate::persistence::xml_repository;

const THEMES_FILE: &str = "themes.json";

fn builtin_themes() -> Vec<Theme> {
    vec![
        Theme {
            id: "builtin-catppuccin-mocha".into(),
            name: "Catppuccin Mocha".into(),
            foreground_color: "#cdd6f4".into(),
            background_color: "#1e1e2e".into(),
            cursor_color: "#f5e0dc".into(),
            selection_color: "#45475a".into(),
            font_family: "JetBrains Mono".into(),
            font_size: 14.0,
            ansi_colors: vec![
                "#45475a".into(),
                "#f38ba8".into(),
                "#a6e3a1".into(),
                "#f9e2af".into(),
                "#89b4fa".into(),
                "#f5c2e7".into(),
                "#94e2d5".into(),
                "#bac2de".into(),
                "#585b70".into(),
                "#f38ba8".into(),
                "#a6e3a1".into(),
                "#f9e2af".into(),
                "#89b4fa".into(),
                "#f5c2e7".into(),
                "#94e2d5".into(),
                "#a6adc8".into(),
            ],
        },
        Theme {
            id: "builtin-dracula".into(),
            name: "Dracula".into(),
            foreground_color: "#f8f8f2".into(),
            background_color: "#282a36".into(),
            cursor_color: "#f8f8f2".into(),
            selection_color: "#44475a".into(),
            font_family: "Fira Code".into(),
            font_size: 14.0,
            ansi_colors: vec![
                "#21222c".into(),
                "#ff5555".into(),
                "#50fa7b".into(),
                "#f1fa8c".into(),
                "#bd93f9".into(),
                "#ff79c6".into(),
                "#8be9fd".into(),
                "#f8f8f2".into(),
                "#6272a4".into(),
                "#ff6e6e".into(),
                "#69ff94".into(),
                "#ffffa5".into(),
                "#d6acff".into(),
                "#ff92df".into(),
                "#a4ffff".into(),
                "#ffffff".into(),
            ],
        },
        Theme {
            id: "builtin-nord".into(),
            name: "Nord".into(),
            foreground_color: "#d8dee9".into(),
            background_color: "#2e3440".into(),
            cursor_color: "#d8dee9".into(),
            selection_color: "#434c5e".into(),
            font_family: "Source Code Pro".into(),
            font_size: 14.0,
            ansi_colors: vec![
                "#3b4252".into(),
                "#bf616a".into(),
                "#a3be8c".into(),
                "#ebcb8b".into(),
                "#81a1c1".into(),
                "#b48ead".into(),
                "#88c0d0".into(),
                "#e5e9f0".into(),
                "#4c566a".into(),
                "#bf616a".into(),
                "#a3be8c".into(),
                "#ebcb8b".into(),
                "#81a1c1".into(),
                "#b48ead".into(),
                "#8fbcbb".into(),
                "#eceff4".into(),
            ],
        },
        Theme {
            id: "builtin-solarized-dark".into(),
            name: "Solarized Dark".into(),
            foreground_color: "#839496".into(),
            background_color: "#002b36".into(),
            cursor_color: "#839496".into(),
            selection_color: "#073642".into(),
            font_family: "Inconsolata".into(),
            font_size: 14.0,
            ansi_colors: vec![
                "#073642".into(),
                "#dc322f".into(),
                "#859900".into(),
                "#b58900".into(),
                "#268bd2".into(),
                "#d33682".into(),
                "#2aa198".into(),
                "#eee8d5".into(),
                "#002b36".into(),
                "#cb4b16".into(),
                "#586e75".into(),
                "#657b83".into(),
                "#839496".into(),
                "#6c71c4".into(),
                "#93a1a1".into(),
                "#fdf6e3".into(),
            ],
        },
        Theme {
            id: "builtin-gruvbox-dark".into(),
            name: "Gruvbox Dark".into(),
            foreground_color: "#ebdbb2".into(),
            background_color: "#282828".into(),
            cursor_color: "#ebdbb2".into(),
            selection_color: "#3c3836".into(),
            font_family: "Hack".into(),
            font_size: 14.0,
            ansi_colors: vec![
                "#282828".into(),
                "#cc241d".into(),
                "#98971a".into(),
                "#d79921".into(),
                "#458588".into(),
                "#b16286".into(),
                "#689d6a".into(),
                "#a89984".into(),
                "#928374".into(),
                "#fb4934".into(),
                "#b8bb26".into(),
                "#fabd2f".into(),
                "#83a598".into(),
                "#d3869b".into(),
                "#8ec07c".into(),
                "#ebdbb2".into(),
            ],
        },
        Theme {
            id: "builtin-one-dark".into(),
            name: "One Dark".into(),
            foreground_color: "#abb2bf".into(),
            background_color: "#282c34".into(),
            cursor_color: "#528bff".into(),
            selection_color: "#3e4451".into(),
            font_family: "JetBrains Mono".into(),
            font_size: 14.0,
            ansi_colors: vec![
                "#282c34".into(),
                "#e06c75".into(),
                "#98c379".into(),
                "#e5c07b".into(),
                "#61afef".into(),
                "#c678dd".into(),
                "#56b6c2".into(),
                "#abb2bf".into(),
                "#545862".into(),
                "#e06c75".into(),
                "#98c379".into(),
                "#e5c07b".into(),
                "#61afef".into(),
                "#c678dd".into(),
                "#56b6c2".into(),
                "#c8ccd4".into(),
            ],
        },
        Theme {
            id: "builtin-tokyo-night".into(),
            name: "Tokyo Night".into(),
            foreground_color: "#a9b1d6".into(),
            background_color: "#1a1b26".into(),
            cursor_color: "#c0caf5".into(),
            selection_color: "#33467c".into(),
            font_family: "Cascadia Code".into(),
            font_size: 14.0,
            ansi_colors: vec![
                "#15161e".into(),
                "#f7768e".into(),
                "#9ece6a".into(),
                "#e0af68".into(),
                "#7aa2f7".into(),
                "#bb9af7".into(),
                "#7dcfff".into(),
                "#a9b1d6".into(),
                "#414868".into(),
                "#f7768e".into(),
                "#9ece6a".into(),
                "#e0af68".into(),
                "#7aa2f7".into(),
                "#bb9af7".into(),
                "#7dcfff".into(),
                "#c0caf5".into(),
            ],
        },
        Theme {
            id: "builtin-monokai".into(),
            name: "Monokai Pro".into(),
            foreground_color: "#fcfcfa".into(),
            background_color: "#2d2a2e".into(),
            cursor_color: "#fcfcfa".into(),
            selection_color: "#403e41".into(),
            font_family: "Fira Code".into(),
            font_size: 14.0,
            ansi_colors: vec![
                "#403e41".into(),
                "#ff6188".into(),
                "#a9dc76".into(),
                "#ffd866".into(),
                "#fc9867".into(),
                "#ab9df2".into(),
                "#78dce8".into(),
                "#fcfcfa".into(),
                "#727072".into(),
                "#ff6188".into(),
                "#a9dc76".into(),
                "#ffd866".into(),
                "#fc9867".into(),
                "#ab9df2".into(),
                "#78dce8".into(),
                "#fcfcfa".into(),
            ],
        },
        Theme {
            id: "builtin-material".into(),
            name: "Material Dark".into(),
            foreground_color: "#eeffff".into(),
            background_color: "#263238".into(),
            cursor_color: "#ffcc00".into(),
            selection_color: "#344046".into(),
            font_family: "Roboto Mono".into(),
            font_size: 14.0,
            ansi_colors: vec![
                "#546e7a".into(),
                "#ff5370".into(),
                "#c3e88d".into(),
                "#ffcb6b".into(),
                "#82aaff".into(),
                "#c792ea".into(),
                "#89ddff".into(),
                "#eeffff".into(),
                "#546e7a".into(),
                "#ff5370".into(),
                "#c3e88d".into(),
                "#ffcb6b".into(),
                "#82aaff".into(),
                "#c792ea".into(),
                "#89ddff".into(),
                "#ffffff".into(),
            ],
        },
        Theme {
            id: "builtin-ayu-dark".into(),
            name: "Ayu Dark".into(),
            foreground_color: "#bfbdb6".into(),
            background_color: "#0d1017".into(),
            cursor_color: "#e6b450".into(),
            selection_color: "#1a1f29".into(),
            font_family: "IBM Plex Mono".into(),
            font_size: 14.0,
            ansi_colors: vec![
                "#01060e".into(),
                "#ea6c73".into(),
                "#91b362".into(),
                "#f9af4f".into(),
                "#53bdfa".into(),
                "#fae994".into(),
                "#90e1c6".into(),
                "#c7c7c7".into(),
                "#686868".into(),
                "#f07178".into(),
                "#c2d94c".into(),
                "#ffb454".into(),
                "#59c2ff".into(),
                "#ffee99".into(),
                "#95e6cb".into(),
                "#ffffff".into(),
            ],
        },
        Theme {
            id: "builtin-github-dark".into(),
            name: "GitHub Dark".into(),
            foreground_color: "#c9d1d9".into(),
            background_color: "#0d1117".into(),
            cursor_color: "#58a6ff".into(),
            selection_color: "#1f2937".into(),
            font_family: "Cascadia Code".into(),
            font_size: 14.0,
            ansi_colors: vec![
                "#484f58".into(),
                "#ff7b72".into(),
                "#3fb950".into(),
                "#d29922".into(),
                "#58a6ff".into(),
                "#bc8cff".into(),
                "#39d353".into(),
                "#b1bac4".into(),
                "#6e7681".into(),
                "#ffa198".into(),
                "#56d364".into(),
                "#e3b341".into(),
                "#79c0ff".into(),
                "#d2a8ff".into(),
                "#56d364".into(),
                "#f0f6fc".into(),
            ],
        },
        Theme {
            id: "builtin-everforest".into(),
            name: "Everforest Dark".into(),
            foreground_color: "#d3c6aa".into(),
            background_color: "#2d353b".into(),
            cursor_color: "#d3c6aa".into(),
            selection_color: "#3d484d".into(),
            font_family: "Victor Mono".into(),
            font_size: 14.0,
            ansi_colors: vec![
                "#475258".into(),
                "#e67e80".into(),
                "#a7c080".into(),
                "#dbbc7f".into(),
                "#7fbbb3".into(),
                "#d699b6".into(),
                "#83c092".into(),
                "#d3c6aa".into(),
                "#475258".into(),
                "#e67e80".into(),
                "#a7c080".into(),
                "#dbbc7f".into(),
                "#7fbbb3".into(),
                "#d699b6".into(),
                "#83c092".into(),
                "#d3c6aa".into(),
            ],
        },
    ]
}

fn load_user_themes() -> Vec<Theme> {
    xml_repository::load_json::<Vec<Theme>>(THEMES_FILE)
        .ok()
        .flatten()
        .unwrap_or_default()
}

fn save_user_themes(themes: &[Theme]) -> Result<(), String> {
    xml_repository::save_json(THEMES_FILE, themes).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_themes() -> Result<Vec<Theme>, String> {
    let builtins = builtin_themes();
    let user = load_user_themes();
    let mut all = builtins;
    all.extend(user);
    Ok(all)
}

#[tauri::command]
pub async fn save_theme(theme: Theme) -> Result<(), String> {
    let mut user = load_user_themes();
    if let Some(pos) = user.iter().position(|t| t.id == theme.id) {
        user[pos] = theme;
    } else {
        user.push(theme);
    }
    save_user_themes(&user)
}

#[tauri::command]
pub async fn delete_theme(id: String) -> Result<(), String> {
    if id.starts_with("builtin-") {
        return Err("Cannot delete built-in themes".into());
    }
    let mut user = load_user_themes();
    user.retain(|t| t.id != id);
    save_user_themes(&user)
}

#[tauri::command]
pub async fn get_active_theme_id() -> Result<String, String> {
    let id: Option<String> =
        xml_repository::load_json("active-theme.json").map_err(|e| e.to_string())?;
    Ok(id.unwrap_or_else(|| "builtin-catppuccin-mocha".into()))
}

#[tauri::command]
pub async fn set_active_theme_id(id: String) -> Result<(), String> {
    xml_repository::save_json("active-theme.json", &id).map_err(|e| e.to_string())
}
