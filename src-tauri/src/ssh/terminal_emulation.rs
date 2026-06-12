//! Central mapping between persisted connection values and terminal emulation types.
//!
//! Port of the Java `TerminalEmulationSupport` together with the SithTermFX
//! `EmulationType` table. The stored value is the enum name (e.g. `XTERM`);
//! unknown or missing values resolve to the XTERM default (`xterm-256color`).

use serde::Serialize;

/// One supported terminal emulation with its metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalEmulation {
    /// Stable enum name used as the persisted value (Java `EmulationType.name()`).
    pub name: &'static str,
    /// Value sent as TERM in the SSH PTY request.
    pub term_name: &'static str,
    /// Human readable name shown in the UI.
    pub display_name: &'static str,
}

/// Default emulation (Java `TerminalEmulationSupport.DEFAULT_EMULATION` = XTERM).
pub const DEFAULT_EMULATION: TerminalEmulation = TerminalEmulation {
    name: "XTERM",
    term_name: "xterm-256color",
    display_name: "XTerm (256 color)",
};

/// All supported emulations in the exact order of the Java `EmulationType` enum.
pub const AVAILABLE_EMULATIONS: &[TerminalEmulation] = &[
    DEFAULT_EMULATION,
    TerminalEmulation {
        name: "VT100",
        term_name: "vt100",
        display_name: "DEC VT100",
    },
    TerminalEmulation {
        name: "VT102",
        term_name: "vt102",
        display_name: "DEC VT102",
    },
    TerminalEmulation {
        name: "VT220",
        term_name: "vt220",
        display_name: "DEC VT220",
    },
    TerminalEmulation {
        name: "VT320",
        term_name: "vt320",
        display_name: "DEC VT320",
    },
    TerminalEmulation {
        name: "VT420",
        term_name: "vt420",
        display_name: "DEC VT420",
    },
    TerminalEmulation {
        name: "VT520",
        term_name: "vt520",
        display_name: "DEC VT520",
    },
    TerminalEmulation {
        name: "TN3270",
        term_name: "IBM-3279-2-E",
        display_name: "IBM TN3270",
    },
    TerminalEmulation {
        name: "TN5250",
        term_name: "IBM-3179-2",
        display_name: "IBM TN5250",
    },
    TerminalEmulation {
        name: "SUN_CDE",
        term_name: "dtterm",
        display_name: "Sun CDE (dtterm)",
    },
    TerminalEmulation {
        name: "WY50",
        term_name: "wy50",
        display_name: "Wyse 50",
    },
    TerminalEmulation {
        name: "WY60",
        term_name: "wy60",
        display_name: "Wyse 60",
    },
    TerminalEmulation {
        name: "WY160",
        term_name: "wy160",
        display_name: "Wyse 160",
    },
    TerminalEmulation {
        name: "TVI910",
        term_name: "tvi910",
        display_name: "TeleVideo 910",
    },
    TerminalEmulation {
        name: "TVI920",
        term_name: "tvi920",
        display_name: "TeleVideo 920",
    },
    TerminalEmulation {
        name: "TVI925",
        term_name: "tvi925",
        display_name: "TeleVideo 925",
    },
    TerminalEmulation {
        name: "HP2392",
        term_name: "hp2392",
        display_name: "HP 2392",
    },
    TerminalEmulation {
        name: "HP700_92",
        term_name: "hp700-92",
        display_name: "HP 700/92",
    },
    TerminalEmulation {
        name: "SCOANSI",
        term_name: "scoansi",
        display_name: "SCO ANSI",
    },
    TerminalEmulation {
        name: "CTERM",
        term_name: "cterm",
        display_name: "DEC CTERM",
    },
    TerminalEmulation {
        name: "PETSCII",
        term_name: "petscii",
        display_name: "Commodore PETSCII",
    },
];

/// Combined display label, Java `TerminalEmulationSupport.displayName`:
/// `"DisplayName (termname)"`.
pub fn display_label(emulation: &TerminalEmulation) -> String {
    format!("{} ({})", emulation.display_name, emulation.term_name)
}

fn normalize(value: &str) -> String {
    value.trim().to_lowercase()
}

/// Exact (normalized) match against enum name, term name, display name or display label.
pub fn find_exact(input: &str) -> Option<&'static TerminalEmulation> {
    let normalized = normalize(input);
    if normalized.is_empty() {
        return None;
    }
    AVAILABLE_EMULATIONS.iter().find(|emulation| {
        normalize(emulation.name) == normalized
            || normalize(emulation.term_name) == normalized
            || normalize(emulation.display_name) == normalized
            || normalize(&display_label(emulation)) == normalized
    })
}

/// Resolves a stored connection value to an emulation; missing, blank or unknown
/// values fall back to the XTERM default (Java `fromStoredValue`).
pub fn from_stored_value(stored: Option<&str>) -> &'static TerminalEmulation {
    match stored {
        Some(value) if !value.trim().is_empty() => find_exact(value).unwrap_or(&DEFAULT_EMULATION),
        _ => &DEFAULT_EMULATION,
    }
}

/// TERM name for a stored connection value; unknown/None resolves to `xterm-256color`.
pub fn term_name_for_stored(stored: Option<&str>) -> &'static str {
    from_stored_value(stored).term_name
}

/// Substring search across enum name, term name, display name and display label
/// (Java `matchesSearch`). Blank queries match everything.
pub fn matches_search(emulation: &TerminalEmulation, query: &str) -> bool {
    let normalized = normalize(query);
    if normalized.is_empty() {
        return true;
    }
    normalize(emulation.name).contains(&normalized)
        || normalize(emulation.term_name).contains(&normalized)
        || normalize(emulation.display_name).contains(&normalized)
        || normalize(&display_label(emulation)).contains(&normalized)
}

/// Serializable list entry for the frontend combo box.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalEmulationInfo {
    pub name: String,
    pub term_name: String,
    pub display_name: String,
}

/// Returns all available terminal emulations for UI selection.
#[tauri::command]
pub fn get_terminal_emulations() -> Vec<TerminalEmulationInfo> {
    AVAILABLE_EMULATIONS
        .iter()
        .map(|emulation| TerminalEmulationInfo {
            name: emulation.name.to_string(),
            term_name: emulation.term_name.to_string(),
            display_name: emulation.display_name.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_emulations_match_sithtermfx_table() {
        let names: Vec<&str> = AVAILABLE_EMULATIONS.iter().map(|e| e.name).collect();
        assert_eq!(
            names,
            vec![
                "XTERM", "VT100", "VT102", "VT220", "VT320", "VT420", "VT520", "TN3270", "TN5250",
                "SUN_CDE", "WY50", "WY60", "WY160", "TVI910", "TVI920", "TVI925", "HP2392",
                "HP700_92", "SCOANSI", "CTERM", "PETSCII",
            ]
        );
    }

    #[test]
    fn defaults_to_xterm_for_missing_or_invalid_values() {
        assert_eq!(from_stored_value(None).name, "XTERM");
        assert_eq!(from_stored_value(Some("")).name, "XTERM");
        assert_eq!(from_stored_value(Some("not-a-terminal")).name, "XTERM");
    }

    #[test]
    fn maps_enum_name_term_name_and_display_label() {
        assert_eq!(from_stored_value(Some("VT220")).name, "VT220");
        assert_eq!(from_stored_value(Some("xterm-256color")).name, "XTERM");
        assert_eq!(
            from_stored_value(Some("XTerm (256 color) (xterm-256color)")).name,
            "XTERM"
        );
    }

    #[test]
    fn search_matches_name_display_name_and_term_name() {
        let vt220 = find_exact("VT220").unwrap();
        let wy60 = find_exact("WY60").unwrap();
        let hp700_92 = find_exact("HP700_92").unwrap();
        let xterm = find_exact("XTERM").unwrap();
        assert!(matches_search(vt220, "vt"));
        assert!(matches_search(wy60, "wy"));
        assert!(matches_search(hp700_92, "hp700"));
        assert!(matches_search(xterm, "xterm-256"));
        assert!(!matches_search(xterm, "wy"));
    }

    #[test]
    fn term_name_uses_selected_emulation() {
        assert_eq!(term_name_for_stored(Some("VT100")), "vt100");
    }

    #[test]
    fn term_name_defaults_to_xterm_256color() {
        assert_eq!(term_name_for_stored(None), "xterm-256color");
    }

    #[test]
    fn display_label_combines_display_and_term_name() {
        assert_eq!(
            display_label(&DEFAULT_EMULATION),
            "XTerm (256 color) (xterm-256color)"
        );
    }

    #[test]
    fn command_returns_all_entries_in_order() {
        let entries = get_terminal_emulations();
        assert_eq!(entries.len(), AVAILABLE_EMULATIONS.len());
        assert_eq!(entries[0].name, "XTERM");
        assert_eq!(entries[0].term_name, "xterm-256color");
        assert_eq!(entries[0].display_name, "XTerm (256 color)");
    }
}
