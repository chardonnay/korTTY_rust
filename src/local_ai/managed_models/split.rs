//! Validated groups of privately copied GGUF shards.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Seek};
use std::path::Path;
use std::sync::atomic::AtomicBool;

use crate::local_ai::runtime_package::open_input_parent;
use crate::private_files::{Directory, FileStamp};

use super::{MAX_MODEL_BYTES, check_cancelled, copy_model, gguf, import_name, invalid};

pub(super) const MAX_SHARDS: usize = 128;

/// One sibling set using llama.cpp's canonical `-00001-of-00002.gguf` naming.
pub(super) struct Names<'a> {
    stem: &'a str,
    pub(super) count: usize,
}

impl<'a> Names<'a> {
    pub(super) fn parse(name: &'a str) -> io::Result<Self> {
        let (stem, index, count) = components(name)
            .ok_or_else(|| invalid("Choose a GGUF shard named model-00001-of-00002.gguf"))?;
        if !(2..=MAX_SHARDS).contains(&count) || !(1..=count).contains(&index) {
            return Err(invalid(
                "The GGUF shard number or count is outside its limit",
            ));
        }
        Ok(Self { stem, count })
    }

    pub(super) fn name(&self, index: usize) -> String {
        format!("{}-{:05}-of-{:05}.gguf", self.stem, index + 1, self.count)
    }

    pub(super) fn model_name(&self) -> io::Result<String> {
        import_name(Path::new(&format!("{}.gguf", self.stem)))
    }
}

pub(super) fn components(name: &str) -> Option<(&str, usize, usize)> {
    let name = name.strip_suffix(".gguf")?;
    let (prefix, count) = name.rsplit_once("-of-")?;
    let (stem, index) = prefix.rsplit_once('-')?;
    if stem.is_empty() {
        return None;
    }
    Some((stem, digits(index)?, digits(count)?))
}

fn digits(value: &str) -> Option<usize> {
    (value.len() == 5 && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse().expect("five ASCII digits fit a usize"))
}

pub(super) fn is_candidate(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .and_then(components)
        .is_some_and(|(_, _, count)| count > 1)
}

pub(super) fn private_name(index: usize, count: usize) -> String {
    Names {
        stem: "model",
        count,
    }
    .name(index)
}

pub(super) fn is_private_name(name: &str) -> bool {
    matches!(components(name), Some(("model", index, count))
        if (2..=MAX_SHARDS).contains(&count) && (1..=count).contains(&index))
}

struct Shard {
    file: File,
    stamp: FileStamp,
    size: u64,
}

#[derive(Default)]
pub(super) struct TensorTotals {
    expected: Option<u64>,
    actual: u64,
}

impl TensorTotals {
    pub(super) fn include(&mut self, inspection: &gguf::Inspection) -> io::Result<()> {
        let expected = *self.expected.get_or_insert(inspection.total_tensors);
        if expected != inspection.total_tensors {
            return Err(invalid("GGUF shards disagree about the total tensor count"));
        }
        self.actual = self
            .actual
            .checked_add(inspection.tensors)
            .filter(|&count| count <= expected)
            .ok_or_else(|| invalid("GGUF shards exceed the declared total tensor count"))?;
        Ok(())
    }

    pub(super) fn finish(self) -> io::Result<()> {
        if self.expected != Some(self.actual) {
            return Err(invalid("The GGUF group is missing tensors"));
        }
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct Scan {
    context: Option<u32>,
    tensors: TensorTotals,
    bytes: u64,
}

impl Scan {
    pub(super) fn include(
        &mut self,
        file: &mut File,
        size: u64,
        index: usize,
        count: usize,
        cancelled: &AtomicBool,
    ) -> io::Result<()> {
        self.bytes = self
            .bytes
            .checked_add(size)
            .filter(|&bytes| size != 0 && bytes <= MAX_MODEL_BYTES)
            .ok_or_else(|| invalid("The GGUF shard group exceeds the model import limit"))?;
        let inspection = gguf::inspect_split(file, size, index, count, cancelled)?;
        self.tensors.include(&inspection)?;
        self.context.get_or_insert(inspection.context);
        Ok(())
    }

    pub(super) fn finish(self) -> io::Result<(u64, u32)> {
        self.tensors.finish()?;
        Ok((
            self.bytes,
            self.context
                .expect("validated group contains at least two shards"),
        ))
    }
}

pub(super) struct Group {
    pub(super) name: String,
    pub(super) context: u32,
    pub(super) bytes: u64,
    shards: Vec<Shard>,
}

impl Group {
    pub(super) fn open(selected: &Path, cancelled: &AtomicBool) -> io::Result<Self> {
        let (source, name) = open_input_parent(selected)?;
        let names = Names::parse(name)?;
        let name = names.model_name()?;
        let mut shards = Vec::with_capacity(names.count);
        let mut scan = Scan::default();
        for index in 0..names.count {
            check_cancelled(cancelled)?;
            let mut file = source.open_file(&names.name(index)).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("Could not open GGUF shard {} ({error})", index + 1),
                )
            })?;
            let stamp = FileStamp::read(&file)?;
            let size = file.metadata()?.len();
            scan.include(&mut file, size, index, names.count, cancelled)?;
            stamp.require_unchanged(&file)?;
            file.rewind()?;
            shards.push(Shard { file, stamp, size });
        }
        let (bytes, context) = scan.finish()?;
        Ok(Self {
            name,
            context,
            bytes,
            shards,
        })
    }

    pub(super) fn sizes(&self) -> Vec<u64> {
        self.shards.iter().map(|shard| shard.size).collect()
    }

    pub(super) fn copy_into(
        &mut self,
        stage: &Directory,
        cancelled: &AtomicBool,
    ) -> io::Result<()> {
        let count = self.shards.len();
        for (index, shard) in self.shards.iter_mut().enumerate() {
            check_cancelled(cancelled)?;
            stage.publish_with(&private_name(index, count), |output| {
                shard.stamp.require_unchanged(&shard.file)?;
                copy_model(&mut shard.file, output, shard.size, cancelled)?;
                shard.stamp.require_unchanged(&shard.file)
            })?;
        }
        Ok(())
    }
}

pub(super) fn valid_sizes(sizes: &[u64], model_bytes: u64) -> bool {
    (2..=MAX_SHARDS).contains(&sizes.len())
        && sizes.iter().try_fold(0_u64, |total, &size| {
            total.checked_add(size).filter(|_| size != 0)
        }) == Some(model_bytes)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    fn string(buffer: &mut Vec<u8>, value: &str) {
        buffer.extend_from_slice(&(value.len() as u64).to_le_bytes());
        buffer.extend_from_slice(value.as_bytes());
    }

    pub(crate) fn fixture(index: u16, count: u16, total: i32, metadata_only: bool) -> Vec<u8> {
        let mut file = b"GGUF".to_vec();
        file.extend_from_slice(&3_u32.to_le_bytes());
        file.extend_from_slice(&u64::from(!metadata_only).to_le_bytes());
        file.extend_from_slice(&4_u64.to_le_bytes());
        string(&mut file, "split.no");
        file.extend_from_slice(&2_u32.to_le_bytes());
        file.extend_from_slice(&index.to_le_bytes());
        string(&mut file, "split.count");
        file.extend_from_slice(&2_u32.to_le_bytes());
        file.extend_from_slice(&count.to_le_bytes());
        string(&mut file, "split.tensors.count");
        file.extend_from_slice(&5_u32.to_le_bytes());
        file.extend_from_slice(&total.to_le_bytes());
        string(&mut file, "llama.context_length");
        file.extend_from_slice(&4_u32.to_le_bytes());
        file.extend_from_slice(&4096_u32.to_le_bytes());
        if !metadata_only {
            string(&mut file, &format!("weight.{index}"));
            file.extend_from_slice(&1_u32.to_le_bytes());
            file.extend_from_slice(&1_u64.to_le_bytes());
            file.extend_from_slice(&0_u32.to_le_bytes());
            file.extend_from_slice(&0_u64.to_le_bytes());
        }
        file.resize(file.len().next_multiple_of(32), 0);
        if !metadata_only {
            file.extend_from_slice(&1_f32.to_le_bytes());
        }
        file
    }

    #[test]
    fn canonical_shard_names_and_sizes_are_bounded() {
        let names = Names::parse("Größe-00002-of-00003.gguf").unwrap();
        assert_eq!(names.name(0), "Größe-00001-of-00003.gguf");
        assert_eq!(private_name(2, 3), "model-00003-of-00003.gguf");
        assert!(is_private_name(&private_name(2, 3)));
        for name in [
            "model-00000-of-00002.gguf",
            "model-00003-of-00002.gguf",
            "model-00001-of-00001.gguf",
            "model-00001-of-00129.gguf",
            "model-1-of-2.gguf",
            "model-+0001-of-00002.gguf",
        ] {
            assert!(Names::parse(name).is_err(), "{name}");
        }
        assert!(valid_sizes(&[25, 30], 55));
        assert!(!valid_sizes(&[25, 30], 54));
        assert!(!valid_sizes(&[55, 0], 55));
        assert!(!valid_sizes(&[u64::MAX, 1], 0));
    }

    #[test]
    fn selected_shard_requires_complete_ordered_gguf_metadata() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let first = root.join("fixture-00001-of-00002.gguf");
        let second = root.join("fixture-00002-of-00002.gguf");
        let cancelled = AtomicBool::new(false);
        let first_bytes = fixture(0, 2, 1, true);
        let second_bytes = fixture(1, 2, 1, false);
        std::fs::write(&first, &first_bytes).unwrap();
        assert!(Group::open(&first, &cancelled).is_err());
        std::fs::write(&second, &second_bytes).unwrap();
        let group = Group::open(&second, &cancelled).unwrap();
        assert_eq!(group.name, "fixture");
        assert_eq!(group.context, 4096);
        assert_eq!(
            group.sizes(),
            [first_bytes.len() as u64, second_bytes.len() as u64]
        );
        drop(group);
        for invalid in [
            fixture(0, 2, 1, false),
            fixture(1, 3, 1, false),
            fixture(1, 2, 2, false),
            fixture(1, 2, 1, true),
        ] {
            std::fs::write(&second, invalid).unwrap();
            assert!(Group::open(&second, &cancelled).is_err());
        }
    }

    #[test]
    fn registered_split_model_opens_first_shard_and_leases_the_whole_group() {
        use super::super::{Purpose, Registry};

        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let first = root.join("fixture-00001-of-00002.gguf");
        let second = root.join("fixture-00002-of-00002.gguf");
        let first_bytes = fixture(0, 2, 2, false);
        let second_bytes = fixture(1, 2, 2, false);
        std::fs::write(&first, &first_bytes).unwrap();
        std::fs::write(&second, &second_bytes).unwrap();
        let registry = Registry::new(root);
        let model = registry
            .import(&second, Purpose::Chat, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(
            registry.list().unwrap().as_slice(),
            std::slice::from_ref(&model)
        );
        assert_eq!(
            model.shard_bytes,
            [first_bytes.len() as u64, second_bytes.len() as u64]
        );
        let opened = registry.open(&model.id).unwrap();
        assert!(opened.path.ends_with(private_name(0, 2)));
        assert_eq!(std::fs::read(&opened.path).unwrap(), first_bytes);
        let private_second = opened.path.with_file_name(private_name(1, 2));
        assert_eq!(std::fs::read(&private_second).unwrap(), second_bytes);
        opened.revalidate().unwrap();
        assert_eq!(
            registry.remove(&model.id).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(opened);
        std::fs::remove_file(private_second).unwrap();
        assert!(registry.open(&model.id).is_err());
        registry.remove(&model.id).unwrap();
        assert!(registry.list().unwrap().is_empty());
        assert_eq!(std::fs::read(first).unwrap(), first_bytes);
        assert_eq!(std::fs::read(second).unwrap(), second_bytes);
    }
}
