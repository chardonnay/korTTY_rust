use super::*;

pub(crate) struct Profile(pub(crate) PathBuf);

impl Profile {
    pub(crate) fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "kortty-snippets-{}",
            String::from(SnippetId::new().unwrap())
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    pub(crate) fn path(&self) -> PathBuf {
        self.0.join("snippets.json")
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn input(name: &str) -> Input {
    Input {
        name: name.into(),
        language: "bash".into(),
        category: "operations".into(),
        content: "printf hello\r\n".into(),
    }
}

#[test]
fn snippets_keep_their_ids_and_concurrent_writes_do_not_clobber() {
    let profile = Profile::new();
    let path = profile.path();
    let mut first = Library::open(path.clone()).unwrap();
    let mut stale = Library::open(path.clone()).unwrap();
    let id = first.save(None, input("Check host")).unwrap();
    assert!(stale.save(None, input("Second writer")).is_err());
    stale.reload().unwrap();
    assert_eq!(stale.get(id).unwrap().content(), "printf hello\r\n");
    assert!(first.save(None, input("  CHECK HOST  ")).is_err());
    assert_eq!(first.save(Some(id), input("Renamed")).unwrap(), id);
    assert!(stale.remove(id).is_err());
    stale.reload().unwrap();
    assert_eq!(stale.get(id).unwrap().name(), "Renamed");
    stale.remove(id).unwrap();
    assert!(Library::open(path).unwrap().entries().is_empty());
}

#[test]
fn custom_header_policy_checks_new_identities_on_every_store_write() {
    let profile = Profile::new();
    let path = profile.path();
    let mut library = Library::open(path.clone()).unwrap();
    let header = |name: &str| Input {
        category: "  sCrIpT-HeAdEr  ".into(),
        ..input(name)
    };
    library.allow_custom_script_headers = Some(true);
    let existing = library.save(None, header("Existing header")).unwrap();
    let ordinary = library.save(None, input("Ordinary snippet")).unwrap();
    let baseline = fs::read(&path).unwrap();
    library.allow_custom_script_headers = Some(false);

    for attempt in [
        library.save(None, header("New header")),
        library.save(Some(ordinary), header("Ordinary snippet")),
        library.create_with_id(
            SnippetId::new().unwrap(),
            header("Duplicate header"),
            Metadata::default(),
        ),
    ] {
        assert_eq!(attempt.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }
    let imports = archive::Imports {
        structured: vec![
            archive::ImportedSnippet {
                id: None,
                input: input("Allowed entry"),
                metadata: Metadata::default(),
            },
            archive::ImportedSnippet {
                id: None,
                input: header("Imported header"),
                metadata: Metadata::default(),
            },
        ],
        ..archive::Imports::default()
    };
    assert_eq!(
        library.import_snippets(imports).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(fs::read(&path).unwrap(), baseline);
    assert_eq!(library.entries().len(), 2);

    let updated = Input {
        content: "# edited template".into(),
        ..header("Renamed header")
    };
    assert_eq!(library.save(Some(existing), updated).unwrap(), existing);
    assert_eq!(
        library.get(existing).unwrap().content(),
        "# edited template"
    );
    assert!(library.save(None, input("New ordinary snippet")).is_ok());
    library.remove(existing).unwrap();
    assert_eq!(
        library
            .create_with_id(existing, header("Recreated header"), Metadata::default())
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied,
    );
    assert!(Library::open(path).unwrap().get(existing).is_none());
}

#[test]
fn invalid_or_oversized_data_is_never_replaced() {
    let profile = Profile::new();
    let path = profile.path();
    let mut library = Library::open(path.clone()).unwrap();
    let too_large = Input {
        content: "x".repeat(MAX_CONTENT_BYTES + 1).into(),
        ..input("Too large")
    };
    assert!(library.save(None, too_large).is_err());
    assert!(!path.exists());
    let invalid = b"{invalid JSON";
    profile::save_atomic(&path, invalid).unwrap();
    assert!(library.save(None, input("New")).is_err());
    assert!(Library::open(path.clone()).is_err());
    assert_eq!(fs::read(path).unwrap(), invalid);
}

#[test]
fn variable_exchange_never_applies_part_of_an_invalid_or_stale_import() {
    let profile = Profile::new();
    let path = profile.path();
    let mut library = Library::open(path.clone()).unwrap();
    let mut stale = Library::open(path.clone()).unwrap();
    let variable = |name: String| Variable {
        name,
        value: "stored".into(),
    };
    library
        .save_variable(None, variable("existing".into()))
        .unwrap();
    let original = fs::read(&path).unwrap();
    assert!(
        stale
            .import_variables(vec![variable("stale".into())], true)
            .is_err()
    );
    let too_many = (0..MAX_VARIABLES)
        .map(|index| variable(format!("imported_{index}")))
        .collect();
    assert!(library.import_variables(too_many, false).is_err());
    assert_eq!(library.variables().len(), 1);
    assert_eq!(fs::read(path).unwrap(), original);
}

#[test]
fn variable_defaults_reopen_atomically_and_case_only_renames_do_not_duplicate() {
    let profile = Profile::new();
    let path = profile.path();
    // Existing snippet profiles omit the optional variables field.
    profile::save_atomic(&path, br#"{"version":1,"snippets":[]}"#).unwrap();
    let mut library = Library::open(path.clone()).unwrap();
    let mut stale = Library::open(path.clone()).unwrap();
    let variable = |name: &str, value: &str| Variable {
        name: name.into(),
        value: value.into(),
    };
    library.save_variable(None, variable("ticket", "")).unwrap();
    assert!(library.variables()[0].default_value().is_none());
    assert!(
        library
            .save_variable(None, variable("TICKET", "duplicate"))
            .is_err()
    );
    assert!(
        stale
            .save_variable(None, variable("other", "stale"))
            .is_err()
    );
    library
        .save_variable(Some("TICKET"), variable("Ticket", "line 1\nline 2"))
        .unwrap();
    let mut reopened = Library::open(path).unwrap();
    assert_eq!(reopened.variables().len(), 1);
    assert_eq!(
        reopened.variables()[0].default_value(),
        Some("line 1\nline 2")
    );
    reopened.remove_variable("tIcKeT").unwrap();
    assert!(reopened.variables().is_empty());
}
