//! JSON-pointer helpers for addressing sub-fields inside Object entries.
//!
//! See RFC 6901. The two characters that need escaping inside a path segment
//! are `~` (becomes `~0`) and `/` (becomes `~1`).

/// Translate a `Vec<String>` path into an RFC 6901 JSON Pointer.
///
/// - An empty path yields `""` (the whole document).
/// - Otherwise each segment is escaped (`~` → `~0`, `/` → `~1`) and prefixed
///   with `/`.
#[allow(dead_code)]
pub fn path_to_json_pointer(path: &[String]) -> String {
    let mut out = String::new();
    for seg in path {
        out.push('/');
        out.push_str(&escape_segment(seg));
    }
    out
}

#[allow(dead_code)]
fn escape_segment(seg: &str) -> String {
    // Replace `~` first, otherwise `/` → `~1` would in turn be re-encoded.
    seg.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_path_is_empty_pointer() {
        let p: Vec<String> = vec![];
        assert_eq!(path_to_json_pointer(&p), "");
    }

    #[test]
    fn test_single_segment() {
        let p = vec!["Tls".to_string()];
        assert_eq!(path_to_json_pointer(&p), "/Tls");
    }

    #[test]
    fn test_two_segments() {
        let p = vec!["Tls".to_string(), "CertPath".to_string()];
        assert_eq!(path_to_json_pointer(&p), "/Tls/CertPath");
    }

    #[test]
    fn test_escapes_tilde_and_slash() {
        let p = vec!["weird~key/with-slash".to_string()];
        assert_eq!(path_to_json_pointer(&p), "/weird~0key~1with-slash");
    }

    #[test]
    fn test_escape_order_tilde_before_slash() {
        // Encoding must replace ~ FIRST, then /, otherwise an input "/" becomes
        // "~01" which decodes back to "~1" not "/".
        let p = vec!["~/".to_string()];
        assert_eq!(path_to_json_pointer(&p), "/~0~1");
    }
}
