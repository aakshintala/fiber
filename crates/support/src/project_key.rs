//! Stable persisted project identity keys.

use std::path::Path;

/// The legacy human-readable project slug, with path separators replaced by `-`.
pub fn legacy_project_slug(identity: &Path) -> String {
    identity.to_string_lossy().replace('/', "-")
}

/// The collision-resistant persisted key for an identity path.
pub fn project_key(identity: &Path) -> String {
    let mut slug = legacy_project_slug(identity);
    while slug.len() > 238 {
        slug.pop();
    }
    let hash = crate::sha256_hex(identity.as_os_str().as_encoded_bytes());
    format!("{slug}-{}", &hash[..16])
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{legacy_project_slug, project_key};

    #[test]
    fn project_key_colliding_paths_differ() {
        assert_ne!(
            project_key(Path::new("/a-b")),
            project_key(Path::new("/a/b"))
        );
    }

    #[test]
    fn project_key_fixed_width() {
        assert_eq!(
            project_key(Path::new("/a-b")),
            format!("-a-b-{}", &crate::sha256_hex(b"/a-b")[..16])
        );
    }

    #[test]
    fn project_key_slug_cap_238() {
        let identity = format!("/{}", "x".repeat(260));
        let key = project_key(Path::new(&identity));
        assert_eq!(key.len(), 255);
        assert_eq!(key.as_bytes()[238], b'-');
    }

    #[test]
    fn legacy_slug_keeps_human_readable_path() {
        assert_eq!(legacy_project_slug(Path::new("/a/b")), "-a-b");
    }
}
