# Downloader

[![CI](https://github.com/stevenke1981/Downloader/actions/workflows/ci.yml/badge.svg)](https://github.com/stevenke1981/Downloader/actions/workflows/ci.yml)

**[繁體中文](#繁體中文) | [English](#english)**

![screenshot](docs/screenshot.png)

---

## 繁體中文

Downloader 是用 Rust 寫的多連線下載加速／管理工具，介面用 Slint，支援 Windows、Linux、macOS。

### 功能
- **多連線分段下載**：用 HTTP Range 把檔案切成多段同時下載（每個下載最多 32 條連線，可設定）。先下完的連線會自動把最慢那段切一半接手，不會被單一條慢連線拖住。
- **續傳**：暫停或關掉程式後，進度會存起來，下次開啟會接著下載。會檢查 ETag / Last-Modified，伺服器上的檔案變了就從頭下載；伺服器不支援 Range 時自動改用單一連線。
- **下載佇列**：同時下載數上限、優先順序（低／一般／高）、失敗自動重試（指數退避）。
- **限速**：全域限速和每個下載各自的限速（token bucket）。
- **排程**：只在指定時段（例如 02:00–07:00）下載，跨午夜也可以。
- **代理伺服器**：HTTP、HTTPS、SOCKS5。
- **校驗碼**：下載完可自動驗證 MD5 或 SHA-256。
- **分類**：依副檔名自動分成影片、音樂、壓縮檔、文件、程式、圖片、其他，可選擇存到各分類的子資料夾。
- **介面**：工具列、側邊欄篩選（全部／進行中／已完成／分類）、下載清單（名稱、大小、進度、速度、剩餘時間、狀態）、詳細資料區（每一段的進度圖、連線數、限速、優先順序）。
- **剪貼簿監看**（可選）：複製下載連結時自動跳出新增視窗。
- **多國語言**：繁體中文、簡體中文、English、日本語、한국어、Español、Deutsch、Français，在設定裡切換立即生效。系統語言是中文時預設繁體中文（簡體中文系統預設簡體）。
- **命令列工具 `dlx`**：不需要介面的下載，可續傳。

### 下載
到 [Releases](https://github.com/stevenke1981/Downloader/releases) 下載：
- Windows：`downloader-setup-windows-x64.exe`（安裝程式，繁中／英文）或 `downloader-windows-x64.zip`（免安裝）
- macOS：`downloader-macos-universal-app.zip`（Downloader.app，Intel 和 Apple 晶片都能用）
- Linux：`downloader-linux-x64.tar.gz`（需要 GTK3 才能開啟選擇資料夾的視窗）

程式沒有數位簽章：Windows 第一次執行可能出現 SmartScreen 警告，macOS 請按右鍵 →「打開」。

### 命令列 `dlx`
```
dlx https://example.com/file.iso -o ~/Downloads -c 16 --limit 5M --checksum <sha256>
```
按 Ctrl+C 中斷後，再執行同一個指令就會續傳。

### 尚未支援
- **FTP**：v0.1 還不支援（`ftp://` 連結會被拒絕）。
- **BitTorrent / 磁力連結**：v0.1 不支援。
- **系統匣圖示**：還沒有，關閉視窗就是結束程式（進度會保存）。
- **瀏覽器擴充功能**：還沒有，可以用剪貼簿監看或 `downloader <網址>` 從命令列加入。

### 建置
```
cargo build --release
cargo test --release
```
翻譯檔在 `gui/lang/`，由 `scripts/gen_po.py` 產生。

---

## English

Downloader is a multi-connection download accelerator and manager written in Rust with a Slint GUI, for Windows, Linux and macOS.

### Features
- **Segmented multi-connection downloads** over HTTP/HTTPS using Range requests (up to 32 connections per download). Connections that finish early split the largest remaining segment, so one slow connection never holds up the download.
- **Resume across restarts**: segment state is persisted; ETag / Last-Modified are checked and the download restarts if the file changed. Servers without Range support fall back to a single connection.
- **Queue** with a max-concurrent limit, priorities, and retries with exponential backoff.
- **Speed limits**: global and per download (token bucket).
- **Scheduler**: only download inside a time window (e.g. 02:00–07:00, wraps midnight).
- **Proxy**: HTTP, HTTPS and SOCKS5.
- **Checksum verification**: MD5 or SHA-256.
- **Categories** by file type (video, music, archives, documents, programs, images, other) with optional per-category sub-folders.
- **GUI**: toolbar, sidebar filters, download list (name, size, progress, speed, ETA, status), details pane with a per-segment progress map, add-download and settings dialogs.
- **Clipboard watching** (optional) opens the add dialog when you copy a link.
- **8 languages** switchable at runtime: 繁體中文, 简体中文, English, 日本語, 한국어, Español, Deutsch, Français. Traditional Chinese is the default on Chinese systems (Simplified on zh-CN/zh-SG).
- **`dlx` CLI** for headless, resumable downloads.

### Not yet supported
FTP, BitTorrent/magnet links, a system tray icon, and browser extensions are not part of v0.1.

### License
MIT. This is an independent project; it is not affiliated with Free Download Manager, and all icons are original.
