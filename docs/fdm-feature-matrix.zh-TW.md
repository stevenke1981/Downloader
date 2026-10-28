# Free Download Manager 功能對照

需求來源：[維基百科 Free Download Manager](https://zh.wikipedia.org/zh-tw/Free_Download_Manager)（2026-10-28 讀取，oldid=94816096）。下表以功能用途對齊，並區分內建引擎、外部工具與歷史服務。

| 來源功能 | Downloader 基準 | 本次交付入口／驗收 |
|---|---|---|
| 分頁／依類型組織下載 | 側邊分類、詳細資訊、設定分頁 | 保留既有流程，新增工具分頁 |
| 每項進度與連線狀態 | 進度、分段圖、狀態、錯誤 | 補任務記錄與工具狀態 |
| 檔案預覽 | 完成項目定位檔案 | 補使用預設檢視器開啟 |
| 共享下載資訊 | 尚無明確入口 | 複製來源 URL；不自動上傳檔案 |
| 拖放框 | 浮動精簡視窗 | 主視窗／精簡視窗匯入 torrent/Metalink/URL 清單 |
| HTTP | 內建 HTTP/HTTPS | 回歸既有 Range、Drive、校驗測試 |
| FTP | 未支援 | aria2c 外部引擎；curl 站點列表 |
| RTSP/MMS | 未支援 | FFmpeg 串流擷取；實際來源驗證分開記錄 |
| 批次下載 | 瀏覽器 IPC 批次 | GUI 多行貼上、去重與整批驗證 |
| 分段同時下載 | 內建最多 32 連線、動態切段 | 保留 |
| BitTorrent | 未支援 | aria2c；本專案未使用 libtorrent |
| Metalink 鏡像 | 未支援 | aria2c 的 Metalink 校驗與鏡像 |
| 磁力連結 | 未支援 | aria2c 的 magnet 入口 |
| 網站影音 | 只接收直接影音 URL | 明確選擇影音模式後使用 yt-dlp |
| 中斷續傳 | 內建持久化分段、ETag/Last-Modified | 內建 HTTP 保留；外部依引擎能力 |
| 低／中／高流量模式 | 手動全域與單項限速 | 三組可設定模式與實際 limiter 接線 |
| 剪貼簿匯入 URL 列表 | 單一 URL 監看 | 多行列表與手動貼上 |
| 瀏覽器整合 | Chrome/Brave/Edge MV3、Cookie 暫存 | 保留擴充功能與 IPC 回歸 |
| Internet 遠端控制 | 僅本機 Native Messaging IPC | 預設關閉的 token 驗證 HTTP API；跨網路可透過 SSH tunnel |
| 自訂副檔名資料夾 | 內建類型資料夾 | 自訂副檔名與目的資料夾規則 |
| 計劃任務開始／停止 | 重複時段排程 | 加入持久化單次開始／停止動作 |
| 排程啟動程式／關機 | 未支援 | 使用者明確新增的 argv／關機動作；驗收不真的關機 |
| 站點分析／FTP 客戶端 | 未支援 | FTP 目錄列出並選擇下載 |
| 站點驗證與連線限制 | 瀏覽器 Cookie、全域佇列 | 精確 host profile、記憶體內密碼、每站併發與連線 |
| HTML 遞迴擷取 | 目前頁面的全部連結 | 有深度／頁數界限的同源擷取 |

驗收狀態隨整合更新於 `fdm-validation.md`；「有入口」不能替代實際下載成功證據。Google Video 與 Flash 是來源文章描述的歷史用途，不聲稱能恢復已停止的服務。站點擷取不等於 Google Drive 資料夾列舉。
