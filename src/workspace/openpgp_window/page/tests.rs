use std::fs;

use pgp::composed::SignedPublicKey;
use pgp::types::KeyVersion;

use crate::snippets::{archive::test_openpgp_key, recipients::inspect_certificate};

use super::*;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/.scratch");
        fs::create_dir_all(&parent).unwrap();
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).unwrap();
        let directory = parent.join(format!("recipient-page-{:016x}", u64::from_ne_bytes(nonce)));
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&directory)
                .unwrap();
        }
        #[cfg(not(unix))]
        fs::create_dir(&directory).unwrap();
        Self(directory)
    }

    fn catalog(&self) -> PathBuf {
        self.0.join("openpgp-recipients.json")
    }

    fn page(&self) -> Page {
        let mut page = Page::loading();
        self.finish(&mut page, Action::List(self.catalog()));
        page
    }

    fn certificate(&self) -> PathBuf {
        let cert = SignedPublicKey::from(test_openpgp_key(KeyVersion::V4, 29));
        let path = self.0.join("recipient.asc");
        fs::write(&path, cert.to_armored_bytes(Default::default()).unwrap()).unwrap();
        path
    }

    fn edit(&self, page: &mut Page, message: Message) -> Option<Action> {
        page.update(message, &self.catalog())
    }

    fn finish(&self, page: &mut Page, action: Action) {
        let result = action.run().map_err(|error| error.to_string());
        assert!(result.is_ok());
        let _ = self.edit(page, Message::Finished(result));
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn recipient_page_requires_current_review_and_independent_fingerprint_before_enrollment() {
    let fixture = Fixture::new();
    let mut page = fixture.page();
    let _ = fixture.edit(&mut page, Message::New);
    let _ = fixture.edit(&mut page, Message::Name("Reports".into()));
    let certificate = fixture.certificate();
    let _ = fixture.edit(
        &mut page,
        Message::Certificate(certificate.to_str().unwrap().to_owned()),
    );
    let (revision, requested) = page.begin_preview().unwrap();
    assert_eq!(requested, certificate);
    let preview = inspect_certificate(&requested).unwrap();
    let fingerprint = preview.fingerprint.clone();
    let _ = fixture.edit(
        &mut page,
        Message::PreviewFinished(revision.wrapping_sub(1), Ok(preview.clone())),
    );
    assert!(page.busy());
    assert!(fixture.edit(&mut page, Message::Save).is_none());

    let _ = fixture.edit(&mut page, Message::PreviewFinished(revision, Ok(preview)));
    assert!(page.form.as_ref().unwrap().fingerprint.is_empty());
    assert!(fixture.edit(&mut page, Message::Save).is_none());
    let _ = fixture.edit(&mut page, Message::Fingerprint("0000".into()));
    assert!(fixture.edit(&mut page, Message::Save).is_none());
    let _ = fixture.edit(
        &mut page,
        Message::Fingerprint(format!(
            "{} {}",
            fingerprint[..8].to_ascii_lowercase(),
            fingerprint[8..].to_ascii_lowercase(),
        )),
    );
    let save = fixture.edit(&mut page, Message::Save).unwrap();
    assert!(matches!(&save, Action::SaveReviewed(..)));
    fixture.finish(&mut page, save);
    assert_eq!(page.selected_choice().unwrap().fingerprint(), fingerprint);
}

#[test]
fn recipient_page_keeps_selection_and_confirms_remove_and_dirty_close() {
    let fixture = Fixture::new();
    let certificate = fixture.certificate();
    let preview = inspect_certificate(&certificate).unwrap();
    let mut page = fixture.page();
    let add = Action::Save(
        fixture.catalog(),
        Change {
            previous: None,
            name: "Original".into(),
            fingerprint: preview.fingerprint.clone(),
            certificate: Some(certificate),
        },
    );
    add.run().unwrap();
    let reload = fixture.edit(&mut page, Message::Reload).unwrap();
    fixture.finish(&mut page, reload);
    let original = page.choices[0].clone();
    let _ = fixture.edit(&mut page, Message::Select(original.clone()));

    let _ = fixture.edit(&mut page, Message::Edit);
    let _ = fixture.edit(&mut page, Message::Name("Renamed".into()));
    assert!(!page.request_close());
    let _ = fixture.edit(&mut page, Message::KeepEditing);
    assert!(!page.discard_and_close());
    assert!(page.dirty());
    let rename = fixture.edit(&mut page, Message::Save).unwrap();
    assert!(matches!(&rename, Action::Save(..)));
    fixture.finish(&mut page, rename);

    let reload = fixture.edit(&mut page, Message::Reload).unwrap();
    fixture.finish(&mut page, reload);
    let renamed = page.selected_choice().unwrap();
    assert_eq!(renamed.fingerprint(), original.fingerprint());
    assert_eq!(renamed.name(), "Renamed");
    assert_ne!(renamed, &original);

    let _ = fixture.edit(&mut page, Message::AskDelete);
    assert!(page.cancel_inner());
    assert!(fixture.edit(&mut page, Message::Delete).is_none());
    let _ = fixture.edit(&mut page, Message::AskDelete);
    let remove = fixture.edit(&mut page, Message::Delete).unwrap();
    fixture.finish(&mut page, remove);
    assert!(page.choices.is_empty());
    assert!(page.selected_choice().is_none());

    let _ = fixture.edit(&mut page, Message::New);
    let _ = fixture.edit(&mut page, Message::Name("Unsaved".into()));
    assert!(!page.request_close());
    assert!(page.discard_and_close());
    assert!(!page.dirty());
}
