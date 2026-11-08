use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use super::{UserSid, require_owner_only};
use crate::tree::Directory;

struct Temp(PathBuf);

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn staging_acl_is_protected_on_all_children_and_kept_on_publication() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("kortty-stage-{}-{nonce}", std::process::id()));
    fs::create_dir(&path).unwrap();
    let path = Temp(path.canonicalize().unwrap());
    let parent = Directory::open_path(&path.0).unwrap();
    let stage = parent.create_child(OsStr::new("stage")).unwrap();
    let owner = UserSid::current().unwrap();
    require_owner_only(&stage.0, owner.sid()).unwrap();
    let nested = stage.create_child(OsStr::new("nested")).unwrap();
    let file = nested.create_file(OsStr::new("data")).unwrap();
    require_owner_only(&nested.0, owner.sid()).unwrap();
    require_owner_only(&file, owner.sid()).unwrap();
    drop((file, nested));
    parent.publish(&stage, OsStr::new("published")).unwrap();
    drop(stage);
    let published = Directory::open_path(&path.0.join("published")).unwrap();
    require_owner_only(&published.0, owner.sid()).unwrap();
}
