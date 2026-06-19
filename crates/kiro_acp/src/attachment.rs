//! Prompt attachments (images and other files) and helpers for turning them
//! into ACP content blocks.

use std::path::{Path, PathBuf};

/// A file the user attached to a prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// Absolute path to the file.
    pub path: PathBuf,
    /// Display name (file name).
    pub name: String,
    /// Detected MIME type, if known.
    pub mime: Option<String>,
    /// Whether this is an image (sent as an `image` content block).
    pub is_image: bool,
}

impl Attachment {
    /// Build an attachment from a path, classifying it by extension.
    pub fn from_path(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase());
        let (mime, is_image) = classify(ext.as_deref());
        Self {
            path,
            name,
            mime,
            is_image,
        }
    }
}

fn classify(ext: Option<&str>) -> (Option<String>, bool) {
    match ext {
        Some("png") => (Some("image/png".into()), true),
        Some("jpg") | Some("jpeg") => (Some("image/jpeg".into()), true),
        Some("gif") => (Some("image/gif".into()), true),
        Some("webp") => (Some("image/webp".into()), true),
        Some("bmp") => (Some("image/bmp".into()), true),
        Some("svg") => (Some("image/svg+xml".into()), true),
        Some("pdf") => (Some("application/pdf".into()), false),
        Some("json") => (Some("application/json".into()), false),
        Some("txt") | Some("md") | Some("rs") | Some("js") | Some("ts") | Some("py")
        | Some("toml") | Some("yaml") | Some("yml") | Some("c") | Some("cpp") | Some("h")
        | Some("go") | Some("java") | Some("sh") => (Some("text/plain".into()), false),
        _ => (None, false),
    }
}

/// Produce a `file://` URI for a path.
pub fn file_uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

/// Encode bytes as standard (padded) base64.
pub fn base64_encode(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(T[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(T[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_images_and_files() {
        let png = Attachment::from_path("/tmp/pic.PNG");
        assert!(png.is_image);
        assert_eq!(png.mime.as_deref(), Some("image/png"));
        assert_eq!(png.name, "pic.PNG");

        let doc = Attachment::from_path("/tmp/notes.md");
        assert!(!doc.is_image);
        assert_eq!(doc.mime.as_deref(), Some("text/plain"));

        let unknown = Attachment::from_path("/tmp/data.bin");
        assert!(!unknown.is_image);
        assert_eq!(unknown.mime, None);
    }

    #[test]
    fn base64_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
    }

    #[test]
    fn file_uri_format() {
        assert_eq!(file_uri(Path::new("/tmp/a.txt")), "file:///tmp/a.txt");
    }
}
