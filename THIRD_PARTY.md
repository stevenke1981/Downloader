# Native protocol dependencies

Downloader is MIT licensed. The following libraries implement the new embedded protocol paths; their licenses and copyright notices remain applicable. Exact transitive versions and package checksums are recorded in `Cargo.lock`.

| Library | Version | License | Source |
|---|---|---|---|
| librqbit and librqbit protocol crates | 9.0.1 | Apache-2.0 | https://github.com/ikatson/rqbit/tree/v9.0.1 |
| retina | 0.4.20 | MIT OR Apache-2.0 | https://github.com/scottlamb/retina |
| roxmltree | 0.21.1 | MIT OR Apache-2.0 | https://github.com/RazrFalcon/roxmltree |
| scraper | 0.24.0 | ISC | https://github.com/rust-scraper/scraper |
| aes | 0.8.4 | MIT OR Apache-2.0 | https://github.com/RustCrypto/block-ciphers |
| cbc | 0.1.2 | MIT OR Apache-2.0 | https://github.com/RustCrypto/block-modes |
| image | 0.25.10 | MIT OR Apache-2.0 | https://github.com/image-rs/image |

Copies of the direct native protocol library notices are in `licenses/`. The Rust protocol implementation does not spawn third-party executables or bind C/C++ torrent/media engines. GUI rendering, operating-system integration and TLS cryptographic dependencies may use native system/C/assembly components; this is not a claim that every transitive dependency is written exclusively in Rust.
