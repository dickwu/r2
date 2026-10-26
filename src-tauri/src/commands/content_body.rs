//! The body of an in-app write (the preview editor's save): text as sent, or
//! bytes sent base64-encoded, for edited images and text whose encoding or
//! byte-order mark the editor preserves.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};

/// Exactly one of `content` (UTF-8 text) or `content_base64` (raw bytes).
pub(crate) fn content_body(
    content: Option<String>,
    content_base64: Option<String>,
) -> Result<Vec<u8>, String> {
    match (content, content_base64) {
        (Some(text), None) => Ok(text.into_bytes()),
        (None, Some(encoded)) => BASE64
            .decode(encoded.as_bytes())
            .map_err(|e| format!("Invalid base64 content: {e}")),
        (Some(_), Some(_)) => Err("Send either content or contentBase64, not both".into()),
        (None, None) => Err("No content to upload".into()),
    }
}

/// An `If-Match` value as S3 expects it: the ETag in quotes. Blank means no
/// condition.
pub(crate) fn if_match_header(etag: Option<String>) -> Option<String> {
    let etag = etag?;
    let trimmed = etag.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed == "*" || trimmed.starts_with('"') || trimmed.starts_with("W/") {
        return Some(trimmed.to_string());
    }
    Some(format!("\"{trimmed}\""))
}

#[cfg(test)]
mod tests {
    use super::{content_body, if_match_header};

    #[test]
    fn text_is_sent_as_its_utf8_bytes() {
        assert_eq!(
            content_body(Some("héllo".into()), None).unwrap(),
            "héllo".as_bytes()
        );
    }

    #[test]
    fn base64_carries_bytes_text_cannot() {
        // A UTF-16LE BOM and a PNG signature byte are not valid UTF-8.
        assert_eq!(
            content_body(None, Some("//6JUA==".into())).unwrap(),
            vec![0xff, 0xfe, 0x89, 0x50]
        );
    }

    #[test]
    fn exactly_one_body_is_required() {
        assert!(content_body(None, None).is_err());
        assert!(content_body(Some("a".into()), Some("YQ==".into())).is_err());
        assert!(content_body(None, Some("not base64!".into())).is_err());
    }

    #[test]
    fn if_match_quotes_a_bare_etag_and_keeps_a_quoted_one() {
        assert_eq!(
            if_match_header(Some("abc".into())).as_deref(),
            Some("\"abc\"")
        );
        assert_eq!(
            if_match_header(Some("\"abc\"".into())).as_deref(),
            Some("\"abc\"")
        );
        assert_eq!(if_match_header(Some("  ".into())), None);
        assert_eq!(if_match_header(None), None);
    }
}
