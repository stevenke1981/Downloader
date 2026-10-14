use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Category {
    Video,
    Audio,
    Archive,
    Document,
    Program,
    Image,
    #[default]
    Other,
}

impl Category {
    pub const ALL: [Category; 7] = [
        Category::Video,
        Category::Audio,
        Category::Archive,
        Category::Document,
        Category::Program,
        Category::Image,
        Category::Other,
    ];

    pub fn from_name(file_name: &str) -> Category {
        let lower = file_name.to_ascii_lowercase();
        let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
        match ext {
            "mp4" | "mkv" | "avi" | "mov" | "webm" | "flv" | "wmv" | "m4v" | "mpg" | "mpeg" | "ts" | "3gp" => Category::Video,
            "mp3" | "flac" | "wav" | "aac" | "ogg" | "m4a" | "opus" | "wma" | "ape" => Category::Audio,
            "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "zst" | "tgz" | "txz" | "lz" | "lzma" | "cab" => Category::Archive,
            "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "epub" | "md" | "odt" | "ods" | "rtf" | "csv" => Category::Document,
            "exe" | "msi" | "dmg" | "pkg" | "deb" | "rpm" | "appimage" | "apk" | "iso" | "img" | "bin" | "run" | "sh" => Category::Program,
            "jpg" | "jpeg" | "png" | "gif" | "webp" | "svg" | "bmp" | "tif" | "tiff" | "heic" | "ico" => Category::Image,
            _ => Category::Other,
        }
    }

    pub fn index(self) -> i32 {
        Category::ALL.iter().position(|c| *c == self).unwrap_or(6) as i32
    }

    /// Sub-folder name used when "sort into category folders" is enabled.
    pub fn folder(self) -> &'static str {
        match self {
            Category::Video => "Video",
            Category::Audio => "Music",
            Category::Archive => "Compressed",
            Category::Document => "Documents",
            Category::Program => "Programs",
            Category::Image => "Images",
            Category::Other => "",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn categorize() {
        assert_eq!(Category::from_name("a.MP4"), Category::Video);
        assert_eq!(Category::from_name("x.tar.gz"), Category::Archive);
        assert_eq!(Category::from_name("setup.exe"), Category::Program);
        assert_eq!(Category::from_name("noext"), Category::Other);
    }
}
