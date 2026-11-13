//! A privately copied llama.cpp vision projector component.

use std::fs::File;
use std::io::{self, Seek};
use std::path::Path;
use std::sync::atomic::AtomicBool;

use crate::private_files::{Directory, FileStamp};

use super::{copy_model, gguf, import_name, invalid, open_input};

pub(super) const NAME: &str = "projector.gguf";
pub(super) const MAX_BYTES: u64 = 32 * 1024 * 1024 * 1024;

pub(super) struct Projector {
    source: File,
    stamp: FileStamp,
    pub(super) bytes: u64,
}

impl Projector {
    pub(super) fn open(path: &Path, cancelled: &AtomicBool) -> io::Result<Self> {
        import_name(path)?;
        let mut source = open_input(path)?;
        let stamp = FileStamp::read(&source)?;
        let bytes = source.metadata()?.len();
        if !(1..=MAX_BYTES).contains(&bytes) {
            return Err(invalid(
                "The vision projector exceeds the import size limit",
            ));
        }
        gguf::projector(&mut source, bytes, cancelled)?;
        stamp.require_unchanged(&source)?;
        source.rewind()?;
        Ok(Self {
            source,
            stamp,
            bytes,
        })
    }

    pub(super) fn copy_into(
        &mut self,
        target: &Directory,
        cancelled: &AtomicBool,
    ) -> io::Result<()> {
        target.publish_with(NAME, |output| {
            self.stamp.require_unchanged(&self.source)?;
            copy_model(&mut self.source, output, self.bytes, cancelled)?;
            self.stamp.require_unchanged(&self.source)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::process::Command;

    use super::super::{Model, Purpose, Registry, tests::fixture};
    use super::*;

    fn string(bytes: &mut Vec<u8>, text: &str) {
        bytes.extend_from_slice(&(text.len() as u64).to_le_bytes());
        bytes.extend_from_slice(text.as_bytes());
    }

    fn projector(vision: bool) -> Vec<u8> {
        let mut bytes = b"GGUF".to_vec();
        bytes.extend_from_slice(&3_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u64.to_le_bytes());
        bytes.extend_from_slice(&2_u64.to_le_bytes());
        string(&mut bytes, "general.architecture");
        bytes.extend_from_slice(&8_u32.to_le_bytes());
        string(&mut bytes, "clip");
        string(&mut bytes, "clip.has_vision_encoder");
        bytes.extend_from_slice(&7_u32.to_le_bytes());
        bytes.push(u8::from(vision));
        string(&mut bytes, "clip.weight");
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u64.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u64.to_le_bytes());
        bytes.resize(bytes.len().next_multiple_of(32), 0);
        bytes.extend_from_slice(&1_f32.to_le_bytes());
        bytes
    }

    #[test]
    fn projector_pair_copies_and_leases_both_components_for_chat() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let model_path = root.join("fixture-model.gguf");
        let projector_path = root.join("fixture-projector.gguf");
        let weights = fixture(4096, 0);
        fs::write(&model_path, &weights).unwrap();
        fs::write(&projector_path, projector(false)).unwrap();
        let registry = Registry::new(root.clone());
        let stopped = AtomicBool::new(false);
        assert!(
            registry
                .import_projector_pair(&model_path, &projector_path, &stopped)
                .is_err()
        );
        assert!(registry.list().unwrap().is_empty());

        let vision = projector(true);
        fs::write(&projector_path, &vision).unwrap();
        let model = registry
            .import_projector_pair(&model_path, &projector_path, &stopped)
            .unwrap();
        assert_eq!(model.purpose, Purpose::Chat);
        assert_eq!(model.projector_bytes, Some(vision.len() as u64));
        assert!(
            registry
                .configure(Model {
                    purpose: Purpose::Embedding,
                    ..model.clone()
                })
                .is_err()
        );
        let opened = registry.open(&model.id).unwrap();
        let private_projector = opened.path.with_file_name(NAME);
        assert_eq!(fs::read(&private_projector).unwrap(), vision);
        let mut launch = Command::new("fixture");
        opened.configure_launch(&mut launch).unwrap();
        assert_eq!(
            launch.get_args().collect::<Vec<_>>(),
            [
                OsStr::new("--model"),
                opened.path.as_os_str(),
                OsStr::new("--mmproj"),
                private_projector.as_os_str(),
            ]
        );
        opened.revalidate().unwrap();
        assert_eq!(
            registry.remove(&model.id).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(opened);
        fs::remove_file(private_projector).unwrap();
        assert!(registry.open(&model.id).is_err());
        assert_eq!(
            crate::local_ai::runtime_package::start_model(&root, &model.id, &stopped).unwrap_err(),
            "The registered vision projector is unavailable"
        );
        crate::local_ai::runtime_package::stop_model(&root, &model.id).unwrap();
        registry.remove(&model.id).unwrap();
        assert_eq!(fs::read(model_path).unwrap(), weights);
        assert_eq!(fs::read(projector_path).unwrap(), vision);
    }
}
