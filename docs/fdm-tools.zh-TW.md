# 內建 Rust 下載引擎

所有下載協定均在 Downloader 程序內執行，無 aria2、FFmpeg、yt-dlp、curl 或其他可執行檔 fallback。`dlx --tools` 顯示「built-in Rust」。舊 `DOWNLOADER_ARIA2` 等環境變數不再生效。Windows GUI 仍使用 Slint／Skia；「原生 Rust」指下載實作與內嵌 Rust 協定函式庫，並非宣稱整個桌面相依樹沒有作業系統或 C/C++ 元件。

| 協定 | Rust 實作與範圍 | 續傳／限制 |
|---|---|---|
| HTTP／HTTPS | 原有 Tokio／reqwest 分段引擎 | Range、ETag／Last-Modified、共用動態限速與 checksum |
| FTP | Tokio TCP；USER/PASS、binary、EPSV／PASV、SIZE／MDTM／REST、NLST | 只有來源大小與時間識別一致且 REST 接受才接續；其他情況重新下載；無 FTPS |
| Metalink v3／v4 | roxmltree；HTTP／FTP 多鏡像、多檔案、digest 校驗 | 已驗證的完成檔可保留；HTTP 半成品重抓；無分塊鏡像拼接 |
| Torrent／magnet | 內嵌 librqbit 9.0.1（Apache-2.0） | peer／tracker／DHT／piece SHA-1 校驗與持久化狀態；單任務限速與共用接收量節流 |
| HLS | master／media、byte range、AES-128 identity、直播輪詢；TS／fMP4／AAC／ID3 | 中斷後重新擷取；無 DRM、LL-HLS、discontinuity、外部分離音軌、加密 init map 或轉碼 |
| 網頁影音 | scraper HTML、直接 media／OpenGraph／公開 player JSON URL、HLS | 不執行網頁 JavaScript，不破解 signatureCipher；不保證所有 YouTube 影片或網站 |
| RTSP | retina 0.4.20（MIT OR Apache-2.0）；RTP over TCP H.264、音訊獨立串流 PCMU／PCMA | H.264 優先，輸出 Annex B .h264；G.711 解碼為 PCM16 WAV；不混流；須收到 RTCP BYE 才完成，重新擷取 |
| MMS | Microsoft MMS-over-HTTP（MMSH／MS-WMSP）固定 packet ASF 封裝解析 | 不支援 MMST TCP transport 或可變 packet ASF；不宣稱所有舊伺服器相容 |

每個任務使用專屬 staging：`payload/` 只放可發布內容，`state/` 保留協定與續傳資料。下載成功後先檢查顯式 whole-file checksum，再發布檔案。多檔下載無法用單一 whole-file checksum 驗證，會明確失敗；不發布控制檔。失敗／取消保留 staging，刪除任務會等待 worker 結束後清理。

同一 Torrent 來源若換成不同 info hash，會先捨棄舊 payload；相同 hash 的重啟仍重算 piece hash。Torrent／Metalink 採實際內容檔名，媒體保留瀏覽器建議名稱的主幹並採實際格式的副檔名，避免把 TS 內容存成 `.m3u8`。

HTTP／FTP／HLS 等位元組路徑使用共用全域與單項 limiter。BitTorrent 使用 session cap 加上暫停／恢復式共用節流，已在網路中的 peer blocks 與取樣期間可能造成短暫超量，不宣稱逐封包精確整形。所有等待均可取消。設定的 HTTP／SOCKS proxy 適用 reqwest 的 HTTP descriptor／鏡像／媒體請求；原始 FTP、RTSP 與 Torrent peer／tracker 連線尚未接上自訂 proxy。

瀏覽器 Cookie 與站點帳密只在記憶體中使用，不寫入狀態或記錄。HTTP 子資源只對原始來源傳送授權資料，跨來源鏡像／媒體不繼承。FTP 帳密透過站點設定輸入，來源 URL 不接受內嵌密碼。

GUI 支援批次貼上、拖放 descriptor、FTP 目錄選檔、遞迴同源 HTML 擷取、站點限制、分類資料夾、三組流量模式、一次性排程與 token 遠端 API。遠端 API 預設關閉且綁定 loopback；跨 Internet tunnel 需另設，不把本機測試宣稱為跨網路驗證。

內建預覽解碼 PNG／JPEG／GIF 首幀／BMP／WebP，限制 32 MiB 壓縮檔、8192×8192 尺寸、16 百萬像素輸出與 64 MiB 解碼配置；文字顯示前 256 KiB，HTML 只顯示文字。其他支援媒體／PDF／EPUB 顯示檔案資訊，尚無播放或排版。下載檔案不交給外部檢視器執行。

驗收入口：`powershell -File scripts/verify-fdm.ps1`。它執行 release 建置、Rust loopback 協定測試、GUI 元件測試與 extension 回歸，不需要另外安裝下載工具。實際 GUI 操作、公開網站相容性與其他平台分開記錄於 [驗收紀錄](fdm-validation.md)。
