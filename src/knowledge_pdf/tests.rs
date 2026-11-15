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

fn write_corpus_fixture(file: File) -> io::Result<()> {
    use std::fmt::Write as _;
    use std::io::{BufWriter, Seek as _};

    let mut output = BufWriter::new(file);
    output.write_all(b"%PDF-1.5\n%\xff\xff\xff\xff\n")?;
    let mut offsets = Vec::new();
    for body in [
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Count 1 /Kids [3 0 R] /MediaBox [0 0 612 792] >>",
        "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> /XObject << /Sample 6 0 R >> >> >>",
        "<< /Length 53 >>\nstream\nBT /F1 12 Tf (Corpus boundary text.) Tj ET /Sample Do\nendstream",
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
    ] {
        offsets.push(output.stream_position()?);
        writeln!(output, "{} 0 obj\n{body}\nendobj", offsets.len())?;
    }

    // Leave room for the document structure. The image bytes are streamed to
    // disk so the test can measure the actual worker without a 500 MiB fixture Vec.
    let pixels = vec![127; FIXTURE_IMAGE_BYTES];
    for _ in 0..(MAX_FILE_BYTES / FIXTURE_IMAGE_BYTES - 2) {
        offsets.push(output.stream_position()?);
        writeln!(
            output,
            "{} 0 obj\n<< /Type /XObject /Subtype /Image /Width 1024 /Height 512 /BitsPerComponent 8 /ColorSpace /DeviceGray /Length {} >>\nstream",
            offsets.len(),
            pixels.len()
        )?;
        output.write_all(&pixels)?;
        output.write_all(b"\nendstream\nendobj\n")?;
    }

    let size = offsets.len() + 1;
    let mut xref = format!("xref\n0 {size}\n0000000000 65535 f \n");
    for offset in offsets {
        writeln!(&mut xref, "{offset:010} 00000 n ").unwrap();
    }
    write!(
        &mut xref,
        "trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n"
    )
    .unwrap();
    let offset = (MAX_FILE_BYTES - xref.len() - 10 - b"\n%%EOF\n".len()) as u64;
    let padding = offset.checked_sub(output.stream_position()?).unwrap();
    io::copy(&mut io::repeat(b' ').take(padding), &mut output)?;
    write!(output, "{xref}{offset:010}\n%%EOF\n")?;
    assert_eq!(output.stream_position()?, MAX_FILE_BYTES as u64);
    output.flush()
}

#[test]
fn pdf_input_reader_applies_the_default_and_explicit_byte_limits() {
    let profile = crate::snippets::tests::Profile::new();
    let path = profile.0.join("large.pdf");
    let bytes = large_fixture(DEFAULT_FILE_BYTES + 1);
    std::fs::write(&path, &bytes).unwrap();
    let open = || File::open(&path).unwrap();
    for limit in [DEFAULT_FILE_BYTES as u64, bytes.len() as u64 - 1] {
        let error = extract(&open(), limit, || true).err().unwrap();
        assert!(input_size_exceeded(&error));
        let error = copy_source(read_header(open(), limit).unwrap(), io::sink(), || Ok(()))
            .err()
            .unwrap();
        assert!(input_size_exceeded(&error));
    }
    let reviewed = extract(&open(), bytes.len() as u64, || true).unwrap();
    assert_eq!(reviewed.bytes, bytes.len());
    assert_eq!(reviewed.digest, <[u8; 32]>::from(Sha256::digest(&bytes)));
    assert_eq!(
        reviewed
            .pages
            .iter()
            .map(|page| page.trim())
            .collect::<Vec<_>>(),
        ["Large PDF fixture text.", "", "Last PDF fixture page."]
    );

    // A source that changes length after the metadata check stays bounded by the pipe writer.
    let mut absolute_boundary = bytes;
    absolute_boundary.resize(PREVIOUS_MAX_FILE_BYTES, b' ');
    std::fs::write(&path, &absolute_boundary).unwrap();
    assert_eq!(
        copy_source(
            read_header(open(), PREVIOUS_MAX_FILE_BYTES as u64).unwrap(),
            io::sink(),
            || Ok(()),
        )
        .unwrap()
        .len,
        PREVIOUS_MAX_FILE_BYTES
    );
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(MAX_FILE_BYTES as u64 + 1)
        .unwrap();
    assert_eq!(
        failure_message(&extract(&open(), u64::MAX, || true).err().unwrap()),
        Some(Failure::Limit.message())
    );
    let invalid = run_worker(
        &profile.0.join("unavailable-worker"),
        &b"Not a PDF"[..],
        50,
        Instant::now() + TIMEOUT,
        &|| true,
    );
    assert_eq!(
        failure_message(&invalid.err().unwrap()),
        Some(Failure::Malformed.message())
    );
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
    let profile = crate::snippets::tests::Profile::new();
    let path = profile.0.join("reviewed.pdf");
    std::fs::write(&path, &bytes).unwrap();
    let open = || File::open(&path).unwrap();
    let limit = bytes.len() as u64;
    let extracted = run_worker(&exe, open(), limit, Instant::now() + TIMEOUT, &|| true).unwrap();
    assert_eq!(extracted.pages[2].trim(), "Third");
    assert_eq!(extracted.digest, <[u8; 32]>::from(Sha256::digest(&bytes)));
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let error = run_worker(&exe, open(), limit, Instant::now() + TIMEOUT, &|| {
        calls.fetch_add(1, Ordering::Relaxed) < 2
    })
    .err()
    .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
}

#[test]
#[ignore = "requires KORTTY_PDF_WORKER_EXE pointing to the native kortty desktop executable"]
fn native_pdf_worker_accepts_larger_reviewed_pdf_and_cancels_while_transferring_it() {
    let exe = std::path::PathBuf::from(
        std::env::var_os("KORTTY_PDF_WORKER_EXE").expect("desktop executable"),
    );
    let bytes = large_fixture(PREVIOUS_MAX_FILE_BYTES + 16 * 1024 * 1024);
    assert!(bytes.len() > PREVIOUS_MAX_FILE_BYTES && bytes.len() < MAX_FILE_BYTES);
    let profile = crate::snippets::tests::Profile::new();
    let path = profile.0.join("reviewed-large.pdf");
    std::fs::write(&path, &bytes).unwrap();
    let open = || File::open(&path).unwrap();
    let limit = bytes.len() as u64;
    let extracted = run_worker(&exe, open(), limit, Instant::now() + TIMEOUT, &|| true).unwrap();
    assert_eq!(
        extracted
            .pages
            .iter()
            .map(|page| page.trim())
            .collect::<Vec<_>>(),
        ["Large PDF fixture text.", "", "Last PDF fixture page."]
    );
    assert_eq!(extracted.bytes, bytes.len());
    assert_eq!(extracted.digest, <[u8; 32]>::from(Sha256::digest(&bytes)));

    struct FixtureSource {
        file: File,
        reading_body: Arc<AtomicBool>,
    }
    impl Read for FixtureSource {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if buffer.len() > PDF_HEADER.len() {
                self.reading_body.store(true, Ordering::Release);
                // Allow the worker owner to cancel while a read is in flight.
                thread::sleep(POLL * 3);
            }
            self.file.read(buffer)
        }
    }
    let reading_body = Arc::new(AtomicBool::new(false));
    let source = FixtureSource {
        file: open(),
        reading_body: Arc::clone(&reading_body),
    };
    let cancelled = run_worker(&exe, source, limit, Instant::now() + TIMEOUT, &|| {
        !reading_body.load(Ordering::Acquire)
    });
    assert!(reading_body.load(Ordering::Acquire));
    assert_eq!(cancelled.err().unwrap().kind(), io::ErrorKind::Interrupted);
}

#[test]
#[ignore = "requires native kortty executable and space for two 500 MiB temporary files"]
fn native_pdf_worker_accepts_the_exact_corpus_boundary() {
    let exe = std::path::PathBuf::from(
        std::env::var_os("KORTTY_PDF_WORKER_EXE").expect("desktop executable"),
    );
    let profile = crate::snippets::tests::Profile::new();
    let path = profile.0.join("corpus-boundary.pdf");
    write_corpus_fixture(File::create(&path).unwrap()).unwrap();
    let source = File::open(&path).unwrap();
    let extracted = run_worker(
        &exe,
        source,
        MAX_FILE_BYTES as u64,
        Instant::now() + TIMEOUT,
        &|| true,
    )
    .unwrap();
    assert_eq!(extracted.bytes, MAX_FILE_BYTES);
    assert_eq!(extracted.pages[0].trim(), "Corpus boundary text.");
}
