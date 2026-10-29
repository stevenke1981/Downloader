use crate::checksum::{Algo, Checksum};
use std::collections::HashSet;
use std::path::PathBuf;

pub(super) struct File {
    pub(super) name: PathBuf,
    pub(super) size: Option<u64>,
    pub(super) hashes: Vec<Checksum>,
    pub(super) mirrors: Vec<reqwest::Url>,
}

pub(super) fn parse(text: &str, base: Option<&reqwest::Url>) -> Result<Vec<File>, String> {
    let document =
        roxmltree::Document::parse(text).map_err(|e| format!("invalid Metalink XML: {e}"))?;
    let root = document.root_element();
    if root.tag_name().name() != "metalink"
        || !matches!(
            root.tag_name().namespace(),
            Some("urn:ietf:params:xml:ns:metalink" | "http://www.metalinker.org/")
        )
    {
        return Err("unsupported Metalink root or namespace".into());
    }
    let mut names = HashSet::new();
    let mut files = Vec::new();
    for node in root
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "file")
    {
        let name = node.attribute("name").ok_or("Metalink file has no name")?;
        // Validate both Windows and Unix spelling at the external filename boundary.
        if name.is_empty()
            || name.contains(['\\', ':', '\0'])
            || name.starts_with('/')
            || name.split('/').any(|part| {
                part.is_empty()
                    || part == "."
                    || part == ".."
                    || crate::util::sanitize(part) != part
            })
        {
            return Err("unsafe Metalink file name".into());
        }
        let key = name.to_lowercase();
        if names.iter().any(|other: &String| {
            other == &key
                || other.starts_with(&format!("{key}/"))
                || key.starts_with(&format!("{other}/"))
        }) {
            return Err("conflicting Metalink file names".into());
        }
        names.insert(key);
        let size = node
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "size")
            .map(|n| {
                n.text()
                    .unwrap_or("")
                    .trim()
                    .parse::<u64>()
                    .map_err(|_| "invalid Metalink size")
            })
            .transpose()?;
        let mut hashes = Vec::new();
        for hash in node.descendants().filter(|n| {
            n.is_element()
                && n.tag_name().name() == "hash"
                && n.parent().is_none_or(|p| p.tag_name().name() != "pieces")
        }) {
            if let Some(algo) = hash.attribute("type").and_then(Algo::parse) {
                let expected = hash.text().unwrap_or("").trim().to_ascii_lowercase();
                if expected.len() != algo.hex_len()
                    || !expected.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err("invalid Metalink digest".into());
                }
                hashes.push(Checksum {
                    algo,
                    expected,
                    source: "Metalink".into(),
                });
            }
        }
        let mut mirrors = Vec::new();
        for url in node
            .descendants()
            .filter(|n| n.is_element() && n.tag_name().name() == "url")
        {
            let text = url.text().unwrap_or("").trim();
            let target = base
                .map_or_else(|| reqwest::Url::parse(text), |base| base.join(text))
                .map_err(|_| "invalid Metalink mirror URL")?;
            if !matches!(target.scheme(), "http" | "https" | "ftp") {
                continue;
            }
            if !target.username().is_empty() || target.password().is_some() {
                return Err("Metalink mirror URL must not contain credentials".into());
            }
            let rank = if root.tag_name().namespace() == Some("http://www.metalinker.org/") {
                u32::MAX
                    - url
                        .attribute("preference")
                        .and_then(|v| v.parse::<u32>().ok())
                        .unwrap_or(0)
            } else {
                url.attribute("priority")
                    .and_then(|v| v.parse::<u32>().ok())
                    .unwrap_or(999999)
            };
            mirrors.push((rank, target));
        }
        mirrors.sort_by_key(|m| m.0);
        if mirrors.is_empty() {
            return Err("Metalink file has no supported HTTP/FTP mirror".into());
        }
        files.push(File {
            name: name.into(),
            size,
            hashes,
            mirrors: mirrors.into_iter().map(|(_, u)| u).collect(),
        });
    }
    if files.is_empty() {
        return Err("Metalink contains no files".into());
    }
    Ok(files)
}
