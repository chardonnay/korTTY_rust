use anyhow::Result;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::io::Write;
use std::path::PathBuf;

pub struct TerminalLogger {
    connection_name: String,
    buffer: Vec<u8>,
}

impl TerminalLogger {
    pub fn new(connection_name: &str) -> Self {
        Self {
            connection_name: connection_name.to_string(),
            buffer: Vec::new(),
        }
    }

    pub fn append(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
    }

    pub fn flush_to_file(&mut self) -> Result<PathBuf> {
        let config_dir = crate::persistence::xml_repository::config_dir()?;
        let history_dir = config_dir.join("history");
        if !history_dir.exists() {
            std::fs::create_dir_all(&history_dir)?;
        }

        let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let filename = format!("{}_{}.log.gz", self.connection_name, timestamp);
        let path = history_dir.join(&filename);

        let file = std::fs::File::create(&path)?;
        let mut encoder = GzEncoder::new(file, Compression::default());
        encoder.write_all(&self.buffer)?;
        encoder.finish()?;

        self.buffer.clear();
        Ok(path)
    }
}
