# Free Download Manager 功能對照

需求來源：[維基百科 Free Download Manager](https://zh.wikipedia.org/zh-tw/Free_Download_Manager)（2026-10-28 讀取，oldid=94816096）。以功能用途對齊；2026-10-28 新要求將下載引擎改為程序內 Rust。下列「支援」仍以 [驗收證據](fdm-validation.md) 為準，不能把入口或函式庫能力視為實際測試。

| 來源功能 | Downloader 實作 | 邊界 |
|---|---|---|
| 分頁／依類型組織 | 分類側邊欄、詳細資訊、設定與工具分頁 | 保留既有 GUI |
| 進度、連線、記錄 | 進度／分段圖／狀態與任務記錄 | 記錄上限 100 筆 |
| 預覽 | 內建 Rust 圖片、文字與目錄清單 | 媒體／PDF／EPUB 僅資訊，無播放／排版 |
| 共享下載資訊 | 複製來源 URL | 不自動上傳檔案 |
| 拖放框 | 精簡浮動視窗與 Winit 檔案拖放 | 可匯入 torrent／Metalink／URL 清單 |
| HTTP／HTTPS | Tokio／reqwest Range 分段引擎 | Google Drive 內容驗證保留 |
| FTP／FTP 客戶端 | 原生 Tokio FTP、登入、目錄列出、選檔下載 | 無 FTPS；被動傳輸 |
| RTSP | retina Rust RTSP／RTP over TCP | H.264 Annex B 或音訊獨立串流 PCMU／PCMA 至 PCM16 WAV；無音訊混流；RTCP BYE 完成 |
| MMS | 原生 MMS-over-HTTP ASF 接收 | 無 MMST；舊伺服器相容性另驗 |
| 批次下載 | 多行貼上、去重、整批驗證 | GUI／CLI／擴充功能 |
| 多連線分段 | 既有 HTTP 最多 32 連線、動態切段 | FTP／串流不宣稱 HTTP 式分段 |
| BitTorrent／magnet | 內嵌 librqbit Rust session、tracker／peer／DHT | 不使用 C++ libtorrent；公開 DHT 另驗 |
| Metalink | 原生 v3／v4、多檔、多鏡像、checksum | 無分塊鏡像拼接 |
| 網站影音 | 直接 media／OpenGraph／公開 player URL 與 HLS | 不解密 DRM／JS 簽章；不保證所有 YouTube |
| 斷點續傳 | HTTP validator、FTP SIZE／MDTM／REST、Torrent piece rehash | Metalink HTTP 半成品重抓；串流重新擷取 |
| 低／中／高流量 | 可設定模式、全域與單項 limiter | Torrent 取樣式背壓可能短暫超量 |
| 剪貼簿 URL 列表 | 多行監看與手動貼上 | 可整批加入 |
| 瀏覽器整合 | Chrome／Brave／Edge MV3、Native Messaging、記憶體 Cookie | 不以瀏覽器登入頁當檔案 |
| Internet 遠端控制 | 預設關閉、token 驗證 loopback HTTP API／dlxctl | Internet tunnel 需另設；不宣稱已驗證 |
| 副檔名資料夾 | 類型分類與自訂副檔名路徑規則 | 既有路徑持久化 |
| 排程開始／停止 | 重複時段與持久化單次動作 | consume-before-execute |
| 排程執行程式／關機 | 使用者明確設定 argv／系統關機 | 驗收不實際關機；非下載依賴 |
| 站點驗證／限制 | 精確 origin 帳密、每站併發與連線 | HTTP 子資源不繼承跨來源憑證 |
| HTML 遞迴擷取 | 同源、有深度與頁數上限 | 不等於 Drive 資料夾列舉 |

Flash 與 Google Video 是來源文章的歷史用途，不宣稱恢復已停止的服務。原有外部引擎驗收保留為歷史記錄，不代表本次原生引擎通過。實作規格與分工見 [native-rust-plan.md](native-rust-plan.md)；協定詳細限制見 [fdm-tools.zh-TW.md](fdm-tools.zh-TW.md)。
