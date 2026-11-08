//! A bounded in-memory semantic index over explicitly listed text files.

use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::local_ai::Client;
use crate::private_files::Directory;

pub(super) const MAX_STORES: usize = 32;
const MAX_SOURCES: usize = 32;
const MAX_FILE_BYTES: usize = 128 * 1024;
const CHUNK_BYTES: usize = 4096;
const MAX_CHUNKS: usize = 256;
const MAX_DIMENSIONS: usize = 16_384;
const BATCH_SIZE: usize = 16;
const SEARCH_HITS: usize = 5;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Store {
    pub(super) name: String,
    pub(super) sources: Vec<PathBuf>,
}

impl Store {
    pub(super) fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            sources: Vec::new(),
        }
    }

    pub(super) fn add_source(&mut self, path: PathBuf) -> Result<(), String> {
        source_name(&path).map_err(|error| error.to_string())?;
        if self.sources.len() >= MAX_SOURCES {
            return Err(format!(
                "A knowledge store supports up to {MAX_SOURCES} files"
            ));
        }
        if self.sources.contains(&path) {
            return Err("This source is already in the store".into());
        }
        self.sources.push(path);
        Ok(())
    }

    pub(super) fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty()
            || self.name.len() > 128
            || self.name.chars().any(char::is_control)
            || self.sources.len() > MAX_SOURCES
        {
            return Err("Invalid knowledge store name or number of sources".into());
        }
        for (index, path) in self.sources.iter().enumerate() {
            source_name(path).map_err(|error| error.to_string())?;
            if self.sources[..index].contains(path) {
                return Err("A source appears more than once in a knowledge store".into());
            }
        }
        Ok(())
    }
}

struct Passage {
    name: String,
    line: usize,
    text: String,
}

pub(crate) struct Index {
    entries: Vec<(Passage, Vec<f32>)>,
    dimensions: usize,
}

impl fmt::Debug for Index {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Index")
            .field("passages", &self.len())
            .finish_non_exhaustive()
    }
}

impl Index {
    pub(super) fn build(client: &Client, store: &Store) -> Result<Self, String> {
        if store.sources.len() > MAX_SOURCES {
            return Err("Too many files in the knowledge store".into());
        }
        let mut passages = Vec::new();
        for path in &store.sources {
            check_active(client)?;
            let name = source_name(path).map_err(|error| error.to_string())?;
            let source =
                read_source(path).map_err(|error| format!("Could not read {name} ({error})"))?;
            for (line, text) in chunks(&source) {
                if passages.len() >= MAX_CHUNKS {
                    return Err(format!(
                        "A knowledge index supports up to {MAX_CHUNKS} text passages"
                    ));
                }
                passages.push(Passage {
                    name: name.into(),
                    line,
                    text: text.into(),
                });
            }
        }
        if passages.is_empty() {
            return Err("The source files contain no text".into());
        }
        let mut vectors = Vec::with_capacity(passages.len());
        let mut dimensions = None;
        for batch in passages.chunks(BATCH_SIZE) {
            check_active(client)?;
            let input: Vec<_> = batch.iter().map(|passage| passage.text.as_str()).collect();
            let embeddings = client.embed(&input)?;
            if embeddings.len() != batch.len() {
                return Err("The service returned the wrong number of embedding vectors".into());
            }
            for mut vector in embeddings {
                let expected = *dimensions.get_or_insert(vector.len());
                normalize(&mut vector, expected)?;
                vectors.push(vector);
            }
        }
        check_active(client)?;
        Ok(Self {
            entries: passages.into_iter().zip(vectors).collect(),
            dimensions: dimensions.ok_or("The embedding service returned no vectors")?,
        })
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn search(&self, client: &Client, query: &str) -> Result<Vec<String>, String> {
        if query.trim().is_empty() {
            return Err("Enter a search query".into());
        }
        let mut embeddings = client.embed(&[query])?;
        let [vector] = embeddings.as_mut_slice() else {
            return Err("The service returned the wrong number of query vectors".into());
        };
        normalize(vector, self.dimensions)?;
        check_active(client)?;
        let mut ranked: Vec<_> = self
            .entries
            .iter()
            .map(|(passage, embedding)| (score(vector, embedding), passage))
            .collect();
        ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
        Ok(ranked
            .into_iter()
            .take(SEARCH_HITS)
            .map(|(score, passage)| {
                format!(
                    "{} · line {} · {score:.3}\n{}",
                    passage.name, passage.line, passage.text
                )
            })
            .collect())
    }
}

fn source_name(path: &Path) -> io::Result<&str> {
    let safe = path.is_absolute()
        && !path.components().any(|part| part == Component::ParentDir)
        && path
            .to_str()
            .is_some_and(|value| value.len() <= 4096 && !value.chars().any(char::is_control));
    if safe && let Some(name) = path.file_name().and_then(|name| name.to_str()) {
        return Ok(name);
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "Enter an absolute path to a text file",
    ))
}

fn read_source(path: &Path) -> io::Result<String> {
    let file = open_source(path)?;
    let mut source = String::new();
    file.take((MAX_FILE_BYTES + 1) as u64)
        .read_to_string(&mut source)?;
    if source.len() > MAX_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Text file exceeds 128 KiB",
        ));
    }
    if source
        .chars()
        .any(|value| value.is_control() && !matches!(value, '\n' | '\r' | '\t'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Source contains binary control characters",
        ));
    }
    Ok(source)
}

fn open_source(path: &Path) -> io::Result<File> {
    let name = source_name(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Source has no parent directory"))?;
    Directory::open(parent, false)?.open_file(name)
}

fn chunks(mut input: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut line = 1;
    std::iter::from_fn(move || {
        while !input.is_empty() {
            let boundary = input.floor_char_boundary(CHUNK_BYTES.min(input.len()));
            let end = if boundary < input.len() {
                input[..boundary]
                    .rfind('\n')
                    .map_or(boundary, |offset| offset + 1)
            } else {
                boundary
            };
            let (prefix, rest) = input.split_at(end);
            input = rest;
            let start = line;
            line += prefix.bytes().filter(|&byte| byte == b'\n').count();
            if !prefix.trim().is_empty() {
                return Some((start, prefix.trim_end()));
            }
        }
        None
    })
}

fn check_active(client: &Client) -> Result<(), String> {
    if client.active() {
        Ok(())
    } else {
        Err("The knowledge request was cancelled".into())
    }
}

fn normalize(vector: &mut [f32], expected: usize) -> Result<(), String> {
    if !(1..=MAX_DIMENSIONS).contains(&expected) || vector.len() != expected {
        return Err("The embedding dimensions are invalid or changed".into());
    }
    let sum: f64 = vector.iter().map(|&value| f64::from(value).powi(2)).sum();
    if !sum.is_finite() || sum == 0.0 {
        return Err("The service returned an invalid embedding vector".into());
    }
    let length = sum.sqrt();
    for value in vector {
        *value = (f64::from(*value) / length) as f32;
    }
    Ok(())
}

fn score(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(a, b)| a * b)
        .sum::<f32>()
        .clamp(-1.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knowledge_chunks_preserve_utf8_and_original_lines() {
        let row = "Ü".repeat(CHUNK_BYTES / 2 - 1);
        let input = format!("{row}\n{row}\nlast line");
        let passages: Vec<_> = chunks(&input).collect();
        assert_eq!(
            passages,
            [(1, row.as_str()), (2, row.as_str()), (3, "last line")]
        );
        let long_line = "界".repeat(CHUNK_BYTES);
        assert!(chunks(&long_line).all(|(line, text)| line == 1 && text.len() <= CHUNK_BYTES));
    }

    #[test]
    fn knowledge_ranking_validates_vector_dimensions_and_values() -> Result<(), String> {
        let mut query = [3.0, 4.0];
        normalize(&mut query, 2)?;
        let mut match_vector = [6.0, 8.0];
        normalize(&mut match_vector, 2)?;
        assert!((score(&query, &match_vector) - 1.0).abs() < f32::EPSILON);
        assert!(score(&query, &[-0.8, -0.6]) < 0.0);
        assert!(normalize(&mut [1.0], 2).is_err());
        assert!(normalize(&mut [f32::NAN, 1.0], 2).is_err());
        assert!(normalize(&mut [0.0, 0.0], 2).is_err());
        Ok(())
    }
}
