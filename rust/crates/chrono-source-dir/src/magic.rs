//! Tipo de un fichero por su extensión y por su número mágico (los primeros
//! bytes), sin dependencias. `category` es la familia (`image`, `audio`…) y
//! `format` el formato concreto (`png`, `mp4`…).

/// Bytes de cabecera que se leen como mucho (lo justo para el texto y el `ftyp`).
pub const HEAD_BYTES: usize = 512;

const BY_EXT: &[(&str, &[&str])] = &[
    ("document", &["pdf", "doc", "docx", "odt", "rtf", "txt", "md", "pages", "epub", "tex"]),
    ("sheet", &["xls", "xlsx", "ods", "csv", "tsv", "numbers"]),
    ("slides", &["ppt", "pptx", "odp", "key"]),
    ("image", &["png", "jpg", "jpeg", "gif", "webp", "heic", "heif", "tif", "tiff", "bmp", "svg", "ico", "dng", "cr2", "nef", "arw"]),
    ("audio", &["mp3", "wav", "flac", "aac", "m4a", "ogg", "opus", "aif", "aiff", "wma"]),
    ("video", &["mp4", "mov", "mkv", "avi", "webm", "m4v", "wmv", "flv", "mpg", "mpeg"]),
    ("archive", &["zip", "tar", "gz", "tgz", "bz2", "xz", "7z", "rar", "zst"]),
    ("data", &["json", "jsonl", "ndjson", "parquet", "db", "sqlite", "sqlite3", "log", "xml", "yaml", "yml", "toml"]),
    ("code", &["rs", "py", "js", "ts", "go", "java", "c", "h", "cpp", "hpp", "sh", "rb", "php", "swift", "kt", "cs", "html", "css", "sql", "ipynb"]),
    ("font", &["ttf", "otf", "woff", "woff2"]),
    ("binary", &["exe", "dll", "so", "dylib", "bin", "dmg", "iso", "apk", "class", "o", "a"]),
];

/// Extensiones de formatos de texto: un fichero de texto con otra extensión
/// conocida (un `.pdf` que es HTML) no es lo que dice ser.
pub const TEXT_EXTS: &[&str] = &[
    "txt", "md", "tex", "rtf", "csv", "tsv", "json", "jsonl", "ndjson", "log", "xml", "yaml", "yml", "toml", "svg", "rs", "py", "js",
    "ts", "go", "java", "c", "h", "cpp", "hpp", "sh", "rb", "php", "swift", "kt", "cs", "html", "css", "sql", "ipynb",
];

/// Familia que sugiere la extensión (en minúsculas), si se conoce.
pub fn category_of_ext(ext: &str) -> Option<&'static str> {
    BY_EXT.iter().find(|(_, exts)| exts.contains(&ext)).map(|(c, _)| *c)
}

/// `(formato, familia)` por la cabecera. `ext` desempata contenedores (un
/// `docx` es un zip; un `doc`, un OLE). `None` si no se reconoce.
pub fn sniff(head: &[u8], ext: &str) -> Option<(&'static str, &'static str)> {
    let at = |off: usize, sig: &[u8]| head.len() >= off + sig.len() && &head[off..off + sig.len()] == sig;
    let by_ext = |fallback: (&'static str, &'static str)| -> (&'static str, &'static str) {
        match BY_EXT.iter().find_map(|(c, exts)| exts.iter().find(|e| **e == ext).map(|e| (*e, *c))) {
            Some(found) => found,
            None => fallback,
        }
    };
    Some(if at(0, b"%PDF") {
        ("pdf", "document")
    } else if at(0, b"\x89PNG\r\n\x1a\n") {
        ("png", "image")
    } else if at(0, b"\xff\xd8\xff") {
        ("jpeg", "image")
    } else if at(0, b"GIF87a") || at(0, b"GIF89a") {
        ("gif", "image")
    } else if at(0, b"RIFF") && at(8, b"WEBP") {
        ("webp", "image")
    } else if at(0, b"RIFF") && at(8, b"WAVE") {
        ("wav", "audio")
    } else if at(0, b"RIFF") && at(8, b"AVI ") {
        ("avi", "video")
    } else if at(0, b"FORM") && (at(8, b"AIFF") || at(8, b"AIFC")) {
        ("aiff", "audio")
    } else if at(0, b"II*\0") || at(0, b"MM\0*") {
        // DNG, CR2 y NEF también son TIFF por dentro.
        by_ext(("tiff", "image"))
    } else if at(4, b"ftyp") {
        if at(8, b"heic") || at(8, b"heix") || at(8, b"mif1") || at(8, b"msf1") || at(8, b"avif") {
            ("heic", "image")
        } else if at(8, b"M4A ") || at(8, b"M4B ") {
            ("m4a", "audio")
        } else if at(8, b"qt  ") {
            ("mov", "video")
        } else {
            ("mp4", "video")
        }
    } else if at(0, b"ID3") {
        ("mp3", "audio")
    } else if at(0, b"fLaC") {
        ("flac", "audio")
    } else if at(0, b"OggS") {
        ("ogg", "audio")
    } else if at(0, b"\x1a\x45\xdf\xa3") {
        ("mkv", "video")
    } else if at(0, b"PK\x03\x04") || at(0, b"PK\x05\x06") {
        // docx/xlsx/pptx/odt/epub/apk son zips: manda la extensión.
        by_ext(("zip", "archive"))
    } else if at(0, b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1") {
        by_ext(("ole", "document"))
    } else if at(0, b"\x1f\x8b") {
        ("gzip", "archive")
    } else if at(0, b"7z\xbc\xaf\x27\x1c") {
        ("7z", "archive")
    } else if at(0, b"Rar!") {
        ("rar", "archive")
    } else if at(0, b"BZh") {
        ("bzip2", "archive")
    } else if at(0, b"\xfd7zXZ\0") {
        ("xz", "archive")
    } else if at(0, b"\x28\xb5\x2f\xfd") {
        ("zstd", "archive")
    } else if at(0, b"SQLite format 3\0") {
        ("sqlite", "data")
    } else if at(0, b"\x7fELF") {
        ("elf", "binary")
    } else if at(0, b"\xfe\xed\xfa\xce") || at(0, b"\xfe\xed\xfa\xcf") || at(0, b"\xce\xfa\xed\xfe") || at(0, b"\xcf\xfa\xed\xfe") {
        ("macho", "binary")
    } else if at(0, b"wOFF") || at(0, b"wOF2") || at(0, b"OTTO") {
        ("font", "font")
    } else if at(0, b"{\\rtf") {
        ("rtf", "document")
    } else if !head.is_empty() && !head.contains(&0) && is_mostly_utf8(head) {
        // Texto: la extensión dice cuál (md, csv, json…) si es de texto.
        if TEXT_EXTS.contains(&ext) {
            by_ext(("text", "document"))
        } else {
            ("text", "document")
        }
    } else {
        return None;
    })
}

/// UTF-8 válido salvo, quizá, un carácter partido al final del trozo leído.
fn is_mostly_utf8(b: &[u8]) -> bool {
    match std::str::from_utf8(b) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none() && b.len() - e.valid_up_to() < 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconoce_cabeceras_y_desempata_por_extension() {
        assert_eq!(sniff(b"%PDF-1.7\n", "pdf"), Some(("pdf", "document")));
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n....", "jpg"), Some(("png", "image")));
        assert_eq!(sniff(b"\0\0\0\x18ftypqt  ", "mov"), Some(("mov", "video")));
        assert_eq!(sniff(b"\0\0\0\x18ftypheic", "heic"), Some(("heic", "image")));
        assert_eq!(sniff(b"PK\x03\x04rest", "docx"), Some(("docx", "document")));
        assert_eq!(sniff(b"PK\x03\x04rest", "foo"), Some(("zip", "archive")));
        assert_eq!(sniff(b"ID3\x04", "mp3"), Some(("mp3", "audio")));
        assert_eq!(sniff(b"hola, mundo\n", "md"), Some(("md", "document")));
        assert_eq!(sniff(b"a,b\n1,2\n", "csv"), Some(("csv", "sheet")));
        assert_eq!(sniff(b"sin extension conocida", ""), Some(("text", "document")));
        assert_eq!(sniff(b"<html></html>", "pdf"), Some(("text", "document")));
        assert_eq!(sniff(b"\x00\x01\x02\x03", "dat"), None);
        // Un carácter multibyte cortado al final del trozo leído sigue siendo texto.
        assert_eq!(sniff("añ".as_bytes().split_at(2).0, "txt"), Some(("txt", "document")));
    }
}
