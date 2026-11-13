use super::*;

#[test]
fn sftp_breadcrumbs_keep_exact_remote_bytes_and_native_local_roots() {
    let remote = Location::Remote(b"/home/name with spaces/raw-\xff/leaf".to_vec());
    let links = crumbs(&remote);
    assert_eq!(links[0].label, "/");
    assert_eq!(links[1].target, Location::Remote(b"/home".to_vec()));
    assert_eq!(
        links[3].target,
        Location::Remote(b"/home/name with spaces/raw-\xff".to_vec())
    );
    assert_eq!(links.last().unwrap().target, remote);
    assert!(links[3].label.contains("\\xff"));

    let drive = crumbs(&Location::Remote(b"C:/Users/demo".to_vec()));
    assert_eq!(drive[0].target, Location::Remote(b"C:/".to_vec()));
    assert_eq!(drive[0].label, "C:/");
    assert_eq!(drive.len(), 3);

    let root = std::env::current_dir().unwrap();
    let local = Location::Local(root.join("space folder").join("child"));
    let links = crumbs(&local);
    assert_eq!(links.last().unwrap().target, local);
    assert_eq!(links[links.len() - 2].label, "space folder");
    assert!(matches!(&links[0].target, Location::Local(path) if path.has_root()));

    #[cfg(windows)]
    {
        let unc = Location::Local(std::path::PathBuf::from(r"\\server\share\parent\child"));
        let links = crumbs(&unc);
        assert_eq!(
            links[0].target,
            Location::Local(std::path::PathBuf::from(r"\\server\share\"))
        );
        assert_eq!(links.len(), 3);
    }

    let deep = Location::Remote(format!("/{}", "d/".repeat(MAX_CRUMBS + 10)).into_bytes());
    let links = crumbs(&deep);
    assert_eq!(links.len(), MAX_CRUMBS);
    assert_eq!(links[0].target, Location::Remote(b"/".to_vec()));
    assert!(links[1].label.starts_with("…"));
}
