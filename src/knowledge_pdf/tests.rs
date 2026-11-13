use lopdf::{
    Dictionary, EncryptionState, EncryptionVersion, Object, Permissions, Stream,
    content::{Content, Operation},
    dictionary,
};

use super::*;

const FIXTURE_IMAGE_BYTES: usize = 512 * 1024;

fn document(pages: &[&str]) -> Document {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font = doc.add_object(
        dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" },
    );
    let image = doc.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image", "Width" => 1, "Height" => 1,
            "BitsPerComponent" => 8, "ColorSpace" => "DeviceGray",
        },
        vec![127],
    ));
    let mut kids = Vec::<Object>::new();
    for text in pages {
        let operations = if text.is_empty() {
            vec![Operation::new("Do", vec!["Image".into()])]
        } else {
            vec![
                Operation::new("BT", Vec::new()),
                Operation::new("Tf", vec!["F1".into(), 12.into()]),
                Operation::new("Tj", vec![Object::string_literal(*text)]),
                Operation::new("ET", Vec::new()),
            ]
        };
        let content = doc.add_object(Stream::new(
            dictionary! {},
            Content { operations }.encode().unwrap(),
        ));
        kids.push(
            doc.add_object(dictionary! {
                "Type" => "Page", "Parent" => pages_id, "Contents" => content,
            })
            .into(),
        );
    }
    doc.objects.insert(pages_id, dictionary! {
        "Type" => "Pages", "Count" => pages.len() as i64, "Kids" => kids,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font }, "XObject" => dictionary! { "Image" => image } },
    }.into());
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    doc.trailer.set(
        "ID",
        vec![
            Object::string_literal("kortty pdf test"),
            Object::string_literal("kortty pdf test"),
        ],
    );
    doc.compress();
    doc
}

pub(crate) fn fixture(pages: &[&str]) -> Vec<u8> {
    let mut bytes = Vec::new();
    document(pages).save_modern(&mut bytes).unwrap();
    bytes
}

pub(crate) fn large_fixture(min_bytes: usize) -> Vec<u8> {
    assert!(min_bytes <= MAX_FILE_BYTES);
    let mut doc = document(&["Large PDF fixture text.", "", "Last PDF fixture page."]);
    let image_page = doc.get_pages()[&2];
    let contents = doc.get_page_contents(image_page)[0];
    let mut images = Dictionary::new();
    let mut operations = Vec::new();
    // Use real image resources to make the document large without raising any
    // per-stream decompression or extracted text limits.
    let count = min_bytes
        .max(DEFAULT_FILE_BYTES + 1)
        .div_ceil(FIXTURE_IMAGE_BYTES);
    for index in 0..count {
        let name = format!("FixtureImage{index}").into_bytes();
        let image = doc.add_object(
            Stream::new(
                dictionary! {
                    "Type" => "XObject", "Subtype" => "Image", "Width" => 1024, "Height" => 512,
                    "BitsPerComponent" => 8, "ColorSpace" => "DeviceGray",
                },
                vec![index as u8; FIXTURE_IMAGE_BYTES],
            )
            .with_compression(false),
        );
        images.set(name.clone(), image);
        operations.push(Operation::new("Do", vec![Object::Name(name)]));
    }
    doc.get_dictionary_mut(image_page)
        .unwrap()
        .set("Resources", dictionary! { "XObject" => images });
    doc.objects.insert(
        contents,
        Stream::new(dictionary! {}, Content { operations }.encode().unwrap()).into(),
    );
    let mut bytes = Vec::new();
    doc.save_modern(&mut bytes).unwrap();
    assert!(bytes.len() >= min_bytes && bytes.len() > DEFAULT_FILE_BYTES);
    bytes
}

#[test]
fn pdf_input_reader_applies_the_default_and_explicit_byte_limits() {
    let profile = crate::snippets::tests::Profile::new();
    let path = profile.0.join("large.pdf");
    let bytes = large_fixture(DEFAULT_FILE_BYTES + 1);
    std::fs::write(&path, &bytes).unwrap();
    let open = || File::open(&path).unwrap();
    for result in [
        read_bytes(&open()),
        read_bytes_with_limit(&open(), bytes.len() as u64 - 1),
    ] {
        assert_eq!(
            failure_message(&result.unwrap_err()),
            Some(Failure::Limit.message())
        );
    }
    let reviewed = read_bytes_with_limit(&open(), bytes.len() as u64).unwrap();
    assert_eq!(reviewed, bytes);
    let pages = parse(&reviewed).unwrap();
    assert_eq!(
        pages.iter().map(|page| page.trim()).collect::<Vec<_>>(),
        ["Large PDF fixture text.", "", "Last PDF fixture page."]
    );

    // The input guard accepts the absolute boundary and clamps a higher caller limit.
    let mut absolute_boundary = bytes;
    absolute_boundary.resize(MAX_FILE_BYTES, b' ');
    std::fs::write(&path, &absolute_boundary).unwrap();
    assert_eq!(
        read_bytes_with_limit(&open(), u64::MAX).unwrap().len(),
        MAX_FILE_BYTES
    );
    absolute_boundary.push(b' ');
    std::fs::write(&path, &absolute_boundary).unwrap();
    assert_eq!(
        failure_message(&read_bytes_with_limit(&open(), u64::MAX).unwrap_err()),
        Some(Failure::Limit.message())
    );
    assert_eq!(parse(&absolute_boundary).unwrap_err(), Failure::Limit);
}

#[test]
fn pdf_extraction_preserves_physical_pages_and_rejects_encryption_or_missing_text() {
    let bytes = fixture(&["First physical page", "", "Third physical page"]);
    let pages = parse(&bytes).unwrap();
    assert_eq!(
        pages.iter().map(|page| page.trim()).collect::<Vec<_>>(),
        ["First physical page", "", "Third physical page"]
    );
    assert_eq!(parse(&fixture(&[""])).unwrap_err(), Failure::NoText);
    assert_eq!(
        parse(b"%PDF-1.7\nnot a PDF").unwrap_err(),
        Failure::Malformed
    );

    for password in ["", "fixture-password"] {
        let mut doc = document(&["Protected text"]);
        let encryption = EncryptionState::try_from(EncryptionVersion::V1 {
            document: &doc,
            owner_password: "fixture-owner-password",
            user_password: password,
            permissions: Permissions::empty(),
        })
        .unwrap();
        doc.encrypt(&encryption).unwrap();
        let mut protected = Vec::new();
        doc.save_to(&mut protected).unwrap();
        assert_eq!(parse(&protected).unwrap_err(), Failure::Protected);
    }
}

#[test]
fn pdf_extraction_rejects_oversized_streams_and_bounded_reply_confusion() {
    let mut doc = document(&["Text"]);
    let content = doc.get_page_contents(doc.get_pages()[&1])[0];
    let mut bomb = Stream::new(dictionary! {}, vec![b' '; MAX_STREAM_BYTES + 1]);
    bomb.compress().unwrap();
    doc.objects.insert(content, bomb.into());
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    assert!(bytes.len() < MAX_FILE_BYTES);
    assert_eq!(parse(&bytes).unwrap_err(), Failure::Limit);

    let pages = vec!["first".into(), String::new(), "third".into()];
    let mut reply = Vec::new();
    encode_reply(&mut reply, Ok(pages.clone())).unwrap();
    assert_eq!(decode_reply(&reply).unwrap(), pages);
    assert!(decode_reply(&reply[..reply.len() - 1]).is_err());
    reply.push(0);
    assert!(decode_reply(&reply).is_err());
    assert!(decode_reply(&[0, 1, 0, 255, 255, 255, 255]).is_err());
}

#[test]
#[ignore = "requires KORTTY_PDF_WORKER_EXE pointing to the native kortty desktop executable"]
fn native_pdf_worker_bounds_output_and_reaps_on_cancellation() {
    let exe = std::path::PathBuf::from(
        std::env::var_os("KORTTY_PDF_WORKER_EXE").expect("desktop executable"),
    );
    let bytes = fixture(&["First", "", "Third"]);
    let extracted = run_worker(&exe, bytes.clone(), Instant::now() + TIMEOUT, &|| true).unwrap();
    assert_eq!(extracted[2].trim(), "Third");
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let error = run_worker(&exe, bytes, Instant::now() + TIMEOUT, &|| {
        calls.fetch_add(1, Ordering::Relaxed) == 0
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
}

#[test]
#[ignore = "requires KORTTY_PDF_WORKER_EXE pointing to the native kortty desktop executable"]
fn native_pdf_worker_accepts_larger_reviewed_pdf_and_cancels_while_transferring_it() {
    let exe = std::path::PathBuf::from(
        std::env::var_os("KORTTY_PDF_WORKER_EXE").expect("desktop executable"),
    );
    let bytes = large_fixture(MAX_FILE_BYTES - FIXTURE_IMAGE_BYTES);
    assert!(bytes.len() < MAX_FILE_BYTES);
    let pages = run_worker(&exe, bytes.clone(), Instant::now() + TIMEOUT, &|| true).unwrap();
    assert_eq!(
        pages.iter().map(|page| page.trim()).collect::<Vec<_>>(),
        ["Large PDF fixture text.", "", "Last PDF fixture page."]
    );
    let checks = std::sync::atomic::AtomicUsize::new(0);
    let cancelled = run_worker(&exe, bytes, Instant::now() + TIMEOUT, &|| {
        checks.fetch_add(1, Ordering::Relaxed) == 0
    });
    assert_eq!(cancelled.unwrap_err().kind(), io::ErrorKind::Interrupted);
}
