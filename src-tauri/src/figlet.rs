use anyhow::Result;

pub fn generate_banner(text: &str, _font: &str) -> Result<String> {
    let fig = figlet_rs::FIGfont::standard().map_err(|e| anyhow::anyhow!("{}", e))?;
    let figure = fig.convert(text).ok_or_else(|| anyhow::anyhow!("Failed to generate banner"))?;
    Ok(figure.to_string())
}

pub fn get_available_fonts() -> Vec<String> {
    vec![
        "standard".into(),
        "3-D".into(),
        "digital".into(),
        "lean".into(),
        "banner".into(),
        "big".into(),
        "block".into(),
        "cosmic".into(),
        "roman".into(),
        "script".into(),
        "small".into(),
    ]
}
