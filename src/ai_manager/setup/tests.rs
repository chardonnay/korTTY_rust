use super::*;

fn scope(profile: &Path) -> Scope<'_> {
    Scope {
        profile,
        ai_allowed: true,
        runtime_downloads_allowed: true,
        model_downloads_allowed: true,
        vault: None,
    }
}

#[test]
fn setup_uses_optional_role_defaults_and_deduplicates_shared_recommendations() {
    let mut wizard = Wizard::with_memory(None, None, None);
    assert_eq!(wizard.unique_models().len(), 2);
    assert_eq!(wizard.choices[0], wizard.choices[1]);
    assert_eq!(wizard.unique_models()[1].purpose, Purpose::Embedding);
    let embedding = Wizard::with_memory(Some(Role::Embedding), None, None);
    assert!(embedding.choices[..2].iter().all(Option::is_none));
    assert_eq!(embedding.unique_models().len(), 1);

    let root = Path::new(".");
    let mut deny = scope(root);
    deny.model_downloads_allowed = false;
    wizard.update(Message::Next, scope(root));
    assert_eq!(wizard.page, Page::Hardware);
    wizard.update(Message::Next, scope(root));
    assert_eq!(wizard.page, Page::Roles);
    wizard.update(Message::Next, deny);
    assert_eq!(wizard.page, Page::Roles);
    assert!(wizard.models.is_empty());
    assert!(!wizard.busy());
}

#[test]
fn guided_setup_uses_the_selected_catalog_snapshot() {
    let catalog = Arc::new(model_catalog::catalog_with_fixture_mapping());
    let mut wizard = Wizard::with_memory(None, None, None);
    wizard.install_model_catalog(Some(catalog));
    assert_eq!(wizard.choices[0].as_ref().unwrap().id, "fixture-model");
    assert_eq!(wizard.choices[0], wizard.choices[1]);
    assert_eq!(wizard.unique_models().len(), 2);
    wizard.page = Page::License;
    wizard.install_model_catalog(None);
    assert_eq!(wizard.choices[0].as_ref().unwrap().id, "fixture-model");
}

#[test]
fn setup_cannot_commit_incomplete_models_or_accept_an_unreviewed_install() {
    let mut wizard = Wizard::with_memory(None, None, None);
    wizard.page = Page::License;
    wizard.models = wizard.unique_models();
    let root = Path::new(".");
    wizard.update(Message::Accept(true), scope(root));
    assert!(!wizard.accepted);
    wizard.update(Message::Next, scope(root));
    assert_eq!(wizard.page, Page::License);
    assert!(wizard.commit(&scope(root)).is_err());
    assert!(!wizard.busy());

    wizard.page = Page::Install;
    wizard.committing = true;
    assert!(!wizard.can_close());
    assert!(matches!(
        wizard.update(Message::Cancel, scope(root)),
        Outcome::Stay(_)
    ));
    wizard.commit_finished(Err("Save failed".into()));
    assert_eq!(wizard.page, Page::Install);
    assert!(wizard.can_close());
    wizard.committing = true;
    wizard.commit_finished(Ok(()));
    assert_eq!(wizard.page, Page::Finish);
    assert!(matches!(
        wizard.update(Message::Finish, scope(root)),
        Outcome::Close
    ));
}
